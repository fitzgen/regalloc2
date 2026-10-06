//! Running the `regicide` register allocator through regalloc2's interface.
//!
//! regicide has its own, richer client interface: register *groups* and banks,
//! rematerialization, explicit critical-edge edits. This module is the shim
//! that adapts a [`Function`][crate::Function] and a
//! [`MachineEnv`][crate::MachineEnv] into a `regicide::Input`, runs regicide
//! over it, and translates the result back into a regalloc2
//! [`Output`][crate::Output].
//!
//! The translation runs on every allocation, so everything that depends only on
//! the machine — and that is most of the work, because describing regalloc2's
//! register model to regicide takes a class per physical register — is built
//! once and cached in [`RegicideCtx`], and every per-run buffer lives there too
//! and is cleared rather than reallocated.
//!
//! # How regalloc2's register model is said in regicide's terms
//!
//! regalloc2 has three fixed register classes and, within each, 64 hardware
//! encodings that a [`MachineEnv`][crate::MachineEnv] either grants to the
//! allocator or does not. regicide instead has banks of registers that the
//! client defines, and expresses every constraint as a *register class*. So:
//!
//! * each regalloc2 class the environment grants at least one register for
//!   becomes one regicide **bank**, whose spill-slot size is that class's
//!   [`Function::spillslot_size`][crate::Function::spillslot_size];
//! * each granted register becomes an allocatable **register** covering one
//!   unit, drawn from a stretch of the global unit space reserved for that
//!   bank so that no two classes alias, plus a **singleton class** naming
//!   only it — which is how an
//!   [`OperandConstraint::FixedReg`][crate::OperandConstraint::FixedReg] and an
//!   [`inst_clobbers`][crate::Function::inst_clobbers] entry are said;
//! * each *un*granted encoding becomes a **non-allocatable register**, again
//!   with a singleton class, so that an operand may still name it — which is
//!   what [`Operand::as_fixed_nonallocatable`][crate::Operand::as_fixed_nonallocatable]
//!   does, and what a `fixed_stack_slots` entry is;
//! * a [`scratch_by_class`][crate::MachineEnv::scratch_by_class] register
//!   becomes an allocatable register that belongs to *no* class, so no value
//!   can be placed in it but regicide may still stage a move through it;
//! * each bank gets a **base class** of everything the allocator may hand out,
//!   in the environment's order of preference, plus one class per
//!   [`OperandConstraint::Limit`][crate::OperandConstraint::Limit] level.

use crate::cfg::CFGInfo;
use crate::{
    Allocation, Block, Ctx, Edit, Function, Inst, MachineEnv, Operand, OperandConstraint,
    OperandKind, OperandPos, PReg, PRegSet, ProgPoint, RegAllocError, RegClass, RegallocOptions,
    SpillSlot, VReg, VecExt,
};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::convert::TryFrom;
use core::ops::Range;
use core::{iter, slice};

use ::regicide as rgc;
use ::regicide::Entity as _;

#[cfg(feature = "regicide-dump")]
mod dump;

/// The number of regalloc2 register classes.
const NUM_CLASSES: usize = 3;

/// How many distinct maxima an
/// [`OperandConstraint::Limit`][crate::OperandConstraint::Limit] can name.
///
/// The encoding requires the maximum to be a power of two no greater than
/// `PReg::MAX + 1`, so the exponent runs over `0..=PReg::MAX_BITS`.
const LIMIT_LEVELS: usize = PReg::MAX_BITS + 1;

/// The spill cost every class this shim defines is given.
///
/// regalloc2's interface says nothing about how expensive spilling one class is
/// relative to another, so there is nothing to tell regicide but "the same".
const SPILL_COST: f32 = 1.0;

/// The `RegClass` with the given index.
fn class_of_index(index: usize) -> RegClass {
    match index {
        0 => RegClass::Int,
        1 => RegClass::Float,
        _ => RegClass::Vector,
    }
}

/// Every register the allocator may hand out for the class with the given
/// index, most preferred first, written into `out`.
fn granted_regs(env: &MachineEnv, index: usize, out: &mut Vec<PReg>) {
    let class = class_of_index(index);
    out.clear();
    let mut seen = PRegSet::empty();
    for set in [
        &env.preferred_regs_by_class[index],
        &env.non_preferred_regs_by_class[index],
    ] {
        for preg in set {
            if preg.class() == class && !seen.contains(preg) {
                seen.add(preg);
                out.push(preg);
            }
        }
    }
}

/// Does the environment grant the allocator any register of the class with the
/// given index, i.e. does the class get a bank?
fn grants_any(env: &MachineEnv, index: usize) -> bool {
    let class = class_of_index(index);
    [
        &env.preferred_regs_by_class[index],
        &env.non_preferred_regs_by_class[index],
    ]
    .iter()
    .flat_map(|set| set.into_iter())
    .any(|preg| preg.class() == class)
}

/// The scratch space a run of regicide needs, kept across runs.
///
/// This hangs off [`Ctx`][crate::Ctx] and is reused rather than rebuilt,
/// the same discipline `Ctx` already applies to ion's own allocations.
#[derive(Default)]
pub(crate) struct RegicideCtx {
    /// regicide's own context, which owns every buffer its passes allocate.
    context: rgc::Context,

    /// The machine description, and the tables for reading regicide's answers
    /// about it back, rebuilt only when the environment actually changes.
    machine: Option<MachineCache>,

    /// Everything the shim works out about the function before handing it over.
    prepass: Prepass,

    /// The regalloc2 spill-slot index of each regicide spill slot.
    spillslot_map: Vec<u32>,
}

/// What the cached [`Machine`][rgc::Machine] was built from.
///
/// Two environments that agree on all of this describe the same machine, so the
/// cached one can be reused. This is compared rather than hashed: it is a few
/// hundred bytes, the comparison stops at the first difference, and it runs
/// once per allocation.
#[derive(PartialEq, Eq)]
struct MachineKey {
    preferred: [PRegSet; NUM_CLASSES],
    non_preferred: [PRegSet; NUM_CLASSES],
    scratch: [Option<PReg>; NUM_CLASSES],

    /// The spill-slot size of each class the environment grants a register
    /// for, and `None` for a class it does not.
    ///
    /// A class with no bank has no answer to give: aarch64, x64 and s390x all
    /// hit an `unreachable!()` when asked about `RegClass::Vector`. So only
    /// ask for a class whose answer gets used.
    spillslot_size: [Option<usize>; NUM_CLASSES],
}

impl MachineKey {
    fn new<F: Function>(func: &F, env: &MachineEnv) -> Self {
        Self {
            preferred: env.preferred_regs_by_class,
            non_preferred: env.non_preferred_regs_by_class,
            scratch: env.scratch_by_class,
            spillslot_size: core::array::from_fn(|i| {
                grants_any(env, i).then(|| func.spillslot_size(class_of_index(i)))
            }),
        }
    }
}

/// A [`Machine`][rgc::Machine] and everything needed to translate between its
/// entities and regalloc2's.
struct MachineCache {
    /// What this was built from; a run whose key differs needs a new one.
    key: MachineKey,

    /// The machine itself.
    machine: rgc::Machine,

    /// Every register the allocator may hand out, including the scratch
    /// registers it may only stage moves through.
    allocatable: PRegSet,

    /// The singleton class naming each `PReg` an operand may be pinned to, or
    /// `None` for one nothing may name — which is to say, a scratch register.
    class_of_preg: [Option<rgc::RegClass>; PReg::NUM_INDEX],

    /// The inverse of `preg_of_reg`: the regicide register naming each `PReg`
    /// the allocator may hand out, and `None` for one it may not.
    reg_of_preg: [Option<rgc::Reg>; PReg::NUM_INDEX],

    /// The `PReg` behind each regicide register, indexed by its `Reg` index.
    ///
    /// This is a `Vec` pushed to as registers are defined rather than an array
    /// indexed by `PReg`, because there is no `PReg` value free to use as a
    /// sentinel: `PReg::invalid()` is `PReg::new(63, RegClass::Int)`, which is
    /// a register a function may perfectly well name.
    preg_of_reg: Vec<PReg>,

    /// Everything the allocator may hand out for each regalloc2 class, in the
    /// environment's order of preference, or `None` for a class it grants
    /// nothing for.
    base_class: [Option<rgc::RegClass>; NUM_CLASSES],

    /// `limit_class[c][k]` names the registers of class `c` whose hardware
    /// encoding is below `1 << k`, or `None` when there are none.
    limit_class: [[Option<rgc::RegClass>; LIMIT_LEVELS]; NUM_CLASSES],
}

/// Report that regicide cannot describe this machine environment.
fn machine_error(error: rgc::Error) -> RegAllocError {
    RegAllocError::RegicideUnsupported(format!(
        "regicide cannot describe this machine environment: {}",
        error
    ))
}

/// Define an allocatable register covering the one unit named by `preg`.
///
/// Register units are a **global** space, not a per-bank one: two registers in
/// different banks interfere exactly when their units overlap
/// (`regicide/src/machine.rs:530-539`), which is how regicide models
/// subregister aliasing. regalloc2's register classes are disjoint physical
/// register files that never alias, so each class's bank gets its own stretch
/// of `PReg::MAX + 1` units and the hardware encoding indexes within it.
fn define_unit_reg(
    builder: &mut rgc::MachineBuilder,
    bank: rgc::RegBank,
    preg: PReg,
) -> Result<rgc::Reg, RegAllocError> {
    let unit =
        |i: usize| rgc::RegUnit::try_new(i).expect("a register unit index fits in a RegUnit");
    let base = preg.class() as usize * (PReg::MAX + 1);
    builder
        .define_reg(
            bank,
            unit(base + preg.hw_enc())..unit(base + preg.hw_enc() + 1),
        )
        .map_err(machine_error)
}

impl MachineCache {
    fn build(key: MachineKey, env: &MachineEnv) -> Result<Self, RegAllocError> {
        let mut builder = rgc::MachineBuilder::new();

        let mut allocatable = PRegSet::empty();
        let mut class_of_preg = [None; PReg::NUM_INDEX];
        let mut reg_of_preg = [None; PReg::NUM_INDEX];
        let mut preg_of_reg = Vec::new();
        let mut base_class = [None; NUM_CLASSES];
        let mut limit_class = [[None; LIMIT_LEVELS]; NUM_CLASSES];

        // Scratch space, reused across the three classes.
        let mut granted: Vec<PReg> = Vec::new();
        let mut regs: Vec<rgc::Reg> = Vec::new();
        let mut members: Vec<rgc::Reg> = Vec::new();

        for c in 0..NUM_CLASSES {
            let class = class_of_index(c);

            // The registers the allocator may hand out, most preferred first.
            granted_regs(env, c, &mut granted);

            // Two spill slots can need a move between them, and no machine can
            // do that without a register to stage it in, so regicide refuses a
            // bank it owns nothing in. A class the environment grants nothing
            // for therefore has no bank at all, and anything naming it is
            // unsupported. `MachineKey::new` asked for the spill-slot size
            // under this same condition, so the size is there when wanted.
            if granted.is_empty() {
                continue;
            }

            let size = key.spillslot_size[c].expect("a granted class was asked its slot size");
            let size = u16::try_from(size)
                .ok()
                .and_then(rgc::SpillSlotSize::new)
                .ok_or_else(|| {
                    RegAllocError::RegicideUnsupported(format!(
                        "a regicide spill slot holds a power-of-two number of \
                         slots, at most {}; {:?} asks for {}",
                        u16::MAX,
                        class,
                        size,
                    ))
                })?;
            let bank = builder.declare_reg_bank(size).map_err(machine_error)?;

            regs.clear();
            for &preg in &granted {
                let reg = define_unit_reg(&mut builder, bank, preg)?;
                preg_of_reg.push(preg);
                reg_of_preg[preg.index()] = Some(reg);
                regs.push(reg);
                allocatable.add(preg);
            }

            // The scratch register, when the client names one: allocatable, so
            // that regicide can stage a move through it, but left out of every
            // class below, so that no value is ever placed in it. That is what
            // `scratch_by_class` means.
            if let Some(scratch) = key.scratch[c] {
                if scratch.class() != class || allocatable.contains(scratch) {
                    return Err(RegAllocError::RegicideUnsupported(format!(
                        "the scratch register for {:?} must belong to that class \
                         and must not also be allocatable",
                        class,
                    )));
                }
                let reg = define_unit_reg(&mut builder, bank, scratch)?;
                preg_of_reg.push(scratch);
                reg_of_preg[scratch.index()] = Some(reg);
                allocatable.add(scratch);
            }

            // Every remaining encoding names a register the allocator does not
            // own but that an instruction may still mention: a fixed
            // non-allocatable operand, or one of the environment's
            // `fixed_stack_slots`. Each gets the singleton class naming it.
            for hw_enc in 0..=PReg::MAX {
                let preg = PReg::new(hw_enc, class);
                if allocatable.contains(preg) {
                    continue;
                }
                let reg = builder
                    .define_non_allocatable_reg(bank)
                    .map_err(machine_error)?;
                preg_of_reg.push(preg);
                class_of_preg[preg.index()] = Some(
                    builder
                        .define_reg_class(bank, SPILL_COST, [reg])
                        .map_err(machine_error)?,
                );
            }

            // One singleton class per granted register, which is how a
            // `FixedReg` constraint and a clobber are said to regicide.
            for (&preg, &reg) in granted.iter().zip(&regs) {
                class_of_preg[preg.index()] = Some(
                    builder
                        .define_reg_class(bank, SPILL_COST, [reg])
                        .map_err(machine_error)?,
                );
            }

            base_class[c] = Some(
                builder
                    .define_reg_class(bank, SPILL_COST, regs.iter().copied())
                    .map_err(machine_error)?,
            );

            // `Limit(1 << k)` admits the registers whose hardware encoding is
            // below `1 << k`. Membership is exactly the encodings under the
            // threshold, so two levels that admit the same *number* of
            // registers admit the same registers and can share a class.
            let mut prev = None;
            let mut prev_len = usize::MAX;
            for k in 0..LIMIT_LEVELS {
                let limit = 1usize << k;
                members.clear();
                members.extend(
                    granted
                        .iter()
                        .zip(&regs)
                        .filter(|(preg, _)| preg.hw_enc() < limit)
                        .map(|(_, &reg)| reg),
                );
                limit_class[c][k] = if members.is_empty() {
                    None
                } else if members.len() == granted.len() {
                    base_class[c]
                } else if members.len() == prev_len {
                    prev
                } else {
                    Some(
                        builder
                            .define_reg_class(bank, SPILL_COST, members.iter().copied())
                            .map_err(machine_error)?,
                    )
                };
                prev = limit_class[c][k];
                prev_len = members.len();
            }
        }

        Ok(MachineCache {
            key,
            machine: builder.finish().map_err(machine_error)?,
            allocatable,
            class_of_preg,
            reg_of_preg,
            preg_of_reg,
            base_class,
            limit_class,
        })
    }

    /// The class naming exactly `preg`, if anything may name it.
    fn singleton(&self, preg: PReg) -> Result<rgc::RegClass, RegAllocError> {
        self.class_of_preg[preg.index()].ok_or_else(|| {
            RegAllocError::RegicideUnsupported(format!(
                "no operand may name {:?}: it is the scratch register for its \
                 class, which the allocator reserves for staging moves",
                preg,
            ))
        })
    }

    /// The class holding everything the allocator may hand out for `class`.
    fn base(&self, class: RegClass) -> Result<rgc::RegClass, RegAllocError> {
        self.base_class[class as usize].ok_or_else(|| {
            RegAllocError::RegicideUnsupported(format!(
                "the machine environment grants the allocator no {:?} register",
                class,
            ))
        })
    }
}

/// Everything the pre-pass works out about one instruction.
#[derive(Clone, Copy)]
struct InstInfo {
    /// Where this instruction's operands start in [`Prepass::operands`].
    ops_start: u32,

    /// How many of them there are.
    num_operands: u32,

    /// The block this instruction lies in.
    block: Block,
}

/// What the shim works out about a function before handing it to regicide.
///
/// Two walks are needed rather than one: regicide numbers values densely and
/// wants every operand's `Val` up front, but a use can precede its definition
/// across a back edge, so no single pass can translate operands as it goes.
#[derive(Default)]
struct Prepass {
    /// The `Val` index of each vreg, or `u32::MAX` for one nothing defines.
    vreg_to_val: Vec<u32>,

    /// The class each `Val` naturally lives in, indexed by `Val`.
    ///
    /// Its length is the function's value count.
    val_class: Vec<rgc::RegClass>,

    /// The vreg each `Val` stands for, the inverse of `vreg_to_val`.
    ///
    /// Needed because regicide names a rematerialized value by `Val` while
    /// `Edit::Remat` names a vreg. Every `Val` here stands for a vreg, since a
    /// clobber becomes a register in a `RegSet` rather than a value.
    val_to_vreg: Vec<VReg>,

    /// What each `Val` costs to recompute, or `None` when it cannot be.
    ///
    /// Held here rather than asked on demand because `AdaptorValue` is
    /// deliberately not generic over the `Function`.
    val_remat: Vec<Option<rgc::RematCost>>,

    /// Every instruction's operands, back to back.
    operands: Vec<rgc::Operand>,

    /// The registers each instruction clobbers, indexed by `Inst`. Held here
    /// because `AdaptorInst` is deliberately not generic over the `Function`,
    /// so it has nothing to ask.
    clobbers: Vec<rgc::RegSet>,

    /// Per-instruction bookkeeping, indexed by `Inst`.
    insts: Vec<InstInfo>,

    /// Which operands of the instruction in hand are already the target of a
    /// reuse, since no two reuses may name the same one.
    reuse_targets: Vec<u32>,
}

/// Everything `define_val` writes, bundled to keep its signature short.
struct ValTables<'a> {
    vreg_to_val: &'a mut [u32],
    val_class: &'a mut Vec<rgc::RegClass>,
    val_to_vreg: &'a mut Vec<VReg>,
    val_remat: &'a mut Vec<Option<rgc::RematCost>>,
}

/// Note the definition of `vreg`, giving it the next `Val`.
///
/// `remat` is what recomputing it costs; see `remat_cost_of_def`.
fn define_val(
    tables: &mut ValTables<'_>,
    machine: &MachineCache,
    multiple_defs_allowed: bool,
    vreg: VReg,
    inst: Inst,
    remat: Option<rgc::RematCost>,
) -> Result<(), RegAllocError> {
    let index = vreg.vreg();
    if index >= tables.vreg_to_val.len() {
        return Err(RegAllocError::SSA(vreg, inst));
    }
    if tables.vreg_to_val[index] != u32::MAX {
        return Err(if multiple_defs_allowed {
            RegAllocError::RegicideUnsupported(String::from(
                "regicide requires every value to be defined exactly once, so \
                 it cannot allocate a function with multiple definitions of a \
                 virtual register",
            ))
        } else {
            RegAllocError::SSA(vreg, inst)
        });
    }
    let class = machine.base(vreg.class())?;
    let val = u32::try_from(tables.val_class.len()).expect("a Val index fits in a u32");
    tables.vreg_to_val[index] = val;
    tables.val_class.push(class);
    tables.val_to_vreg.push(vreg);
    tables.val_remat.push(remat);
    debug_assert_eq!(tables.val_class.len(), tables.val_to_vreg.len());
    debug_assert_eq!(tables.val_class.len(), tables.val_remat.len());
    Ok(())
}

/// What recomputing `vreg` costs, or `None` when nothing could stand in for
/// this definition.
///
/// Only an instruction that reads nothing qualifies, since a remat re-executes
/// it where its inputs are unavailable. The shim checks this rather than
/// trusting the client, because regicide validates it and a violation fails
/// the whole allocation. Its other two rules hold here by construction.
fn remat_cost_of_def<F: Function>(
    func: &F,
    vreg: VReg,
    reads_nothing: bool,
) -> Option<rgc::RematCost> {
    let cost = func.is_rematerializable(vreg);
    debug_assert!(
        cost.is_none() || reads_nothing,
        "a `Function` called a vreg rematerializable whose definition reads \
         other values, which regicide rejects outright; ignoring it",
    );
    if reads_nothing {
        cost
    } else {
        None
    }
}

/// The `Val` standing for `vreg`, or an error if nothing defines it.
fn val_of(vreg_to_val: &[u32], vreg: VReg, inst: Inst) -> Result<rgc::Val, RegAllocError> {
    let val = vreg_to_val.get(vreg.vreg()).copied().unwrap_or(u32::MAX);
    if val == u32::MAX {
        return Err(RegAllocError::SSA(vreg, inst));
    }
    Ok(rgc::Val::try_new(val as usize).expect("Vals are numbered densely from zero"))
}

/// Translate one of `inst`'s operands.
fn translate_operand(
    machine: &MachineCache,
    vreg_to_val: &[u32],
    op: Operand,
    inst: Inst,
) -> Result<rgc::Operand, RegAllocError> {
    let pos = match op.pos() {
        OperandPos::Early => rgc::Position::Early,
        OperandPos::Late => rgc::Position::Late,
    };
    let kind = match op.kind() {
        OperandKind::Def => rgc::DefUse::Def,
        OperandKind::Use => rgc::DefUse::Use,
    };

    // An operand pinned to a register the allocator does not own carries no
    // value at all: it names a location, and regicide has nothing to decide.
    if let Some(preg) = op.as_fixed_nonallocatable() {
        if machine.allocatable.contains(preg) {
            return Err(RegAllocError::RegicideUnsupported(format!(
                "{:?} is a register the allocator owns, so an operand cannot \
                 name it as a fixed non-allocatable register",
                preg,
            )));
        }
        if op.kind() != OperandKind::Use {
            return Err(RegAllocError::RegicideUnsupported(format!(
                "regicide only lets an instruction read a register the \
                 allocator does not own, and this one defines {:?}",
                preg,
            )));
        }
        let class = machine.singleton(preg)?;
        return Ok(rgc::Operand::new(
            pos,
            rgc::DefUse::Use,
            rgc::OperandConstraint::Reg(class),
            None,
        ));
    }

    let vreg = op.vreg();
    let val = val_of(vreg_to_val, vreg, inst)?;
    let constraint = match op.constraint() {
        OperandConstraint::Any => rgc::OperandConstraint::Any,
        OperandConstraint::Reg => rgc::OperandConstraint::Reg(machine.base(vreg.class())?),
        OperandConstraint::Stack => rgc::OperandConstraint::Stack,
        OperandConstraint::FixedReg(preg) => {
            if preg.class() != vreg.class() || !machine.allocatable.contains(preg) {
                return Err(RegAllocError::RegicideUnsupported(format!(
                    "{:?} cannot hold {:?}: it is not a register of that class \
                     that the allocator owns",
                    preg, vreg,
                )));
            }
            rgc::OperandConstraint::Reg(machine.singleton(preg)?)
        }
        // `Operand::constraint` hands back the maximum itself, which the
        // encoding has already forced to be a power of two.
        OperandConstraint::Limit(max) => {
            let level = max.trailing_zeros() as usize;
            let class = machine.limit_class[vreg.class() as usize]
                .get(level)
                .copied()
                .flatten()
                .ok_or_else(|| {
                    RegAllocError::RegicideUnsupported(format!(
                        "the machine environment grants the allocator no {:?} \
                         register below hardware encoding {}",
                        vreg.class(),
                        max,
                    ))
                })?;
            rgc::OperandConstraint::Reg(class)
        }
        OperandConstraint::Reuse(i) => rgc::OperandConstraint::Reuse(i),
    };

    Ok(rgc::Operand::new(pos, kind, constraint, Some(val)))
}

/// Check every reuse constraint among an instruction's operands, and prepare
/// the operands each names.
///
/// regicide accepts a reuse target constrained to either `Any` or `Reg`, but
/// regalloc2's own checker (`checker.rs`, `check_val`) demands that the
/// reusing definition land in a *register* and in the very same one as its
/// target. So a target left at `Any` is upgraded to its value's register class
/// here, which is the constraint that makes regicide's answer one this crate's
/// checker will accept.
fn resolve_reuses(
    val_class: &[rgc::RegClass],
    operands: &mut [rgc::Operand],
    reuse_targets: &mut Vec<u32>,
) -> Result<(), RegAllocError> {
    reuse_targets.clear();

    let bad = |what: &str| {
        Err(RegAllocError::RegicideUnsupported(format!(
            "regicide cannot express this reuse constraint: {}",
            what
        )))
    };

    for i in 0..operands.len() {
        let op = operands[i];
        let target = match op.constraint() {
            rgc::OperandConstraint::Reuse(target) => target,
            _ => continue,
        };

        if op.pos() != rgc::Position::Late || op.kind() != rgc::DefUse::Def {
            return bad("only a late definition may reuse another operand");
        }
        if target >= operands.len() || target == i {
            return bad("it names an operand the instruction does not have");
        }
        if reuse_targets.contains(&(target as u32)) {
            return bad("two definitions reuse the same operand");
        }
        reuse_targets.push(target as u32);

        let mut t = operands[target];
        if t.pos() != rgc::Position::Early || t.kind() != rgc::DefUse::Use {
            return bad("only an early use may be reused");
        }
        let (def_val, use_val) = match (op.val(), t.val()) {
            (Some(def_val), Some(use_val)) => (def_val, use_val),
            _ => return bad("a reuse must be between two operands that name values"),
        };
        if val_class[def_val.index()] != val_class[use_val.index()] {
            return bad("the two operands' values are of different classes");
        }

        match t.constraint() {
            rgc::OperandConstraint::Reg(_) => continue,
            rgc::OperandConstraint::Any => {}
            _ => return bad("the reused operand must be unconstrained or in a register"),
        }

        // Pin the target to a register, so that the definition reusing it
        // lands in one too.
        t = rgc::Operand::new(
            t.pos(),
            t.kind(),
            rgc::OperandConstraint::Reg(val_class[use_val.index()]),
            Some(use_val),
        );
        operands[target] = t;
    }

    Ok(())
}

/// Work out everything regicide will ask about `func`.
fn build_prepass<F: Function>(
    prepass: &mut Prepass,
    func: &F,
    cfginfo: &CFGInfo,
    machine: &MachineCache,
) -> Result<(), RegAllocError> {
    let num_insts = func.num_insts();
    let num_blocks = func.num_blocks();
    let multiple_defs_allowed = func.allow_multiple_vreg_defs();

    // regicide has no notion of an unreachable block: it asks every block for
    // an immediate dominator and cross-checks the answer. `CFGInfo` does not
    // rule these out — `postorder` simply leaves them out — so check here.
    if cfginfo.postorder.len() != num_blocks {
        return Err(RegAllocError::RegicideUnsupported(String::from(
            "regicide requires every block to be reachable from the entry block",
        )));
    }

    // Nothing defines a parameter of the entry block. This is what
    // `validate_ssa` says about one too.
    if !func.block_params(func.entry_block()).is_empty() {
        return Err(RegAllocError::BB(func.entry_block()));
    }

    // Bail before building anything if the function does not fit in regicide's
    // entity numbering. Asking for one more than the count is conservative by
    // one, and simpler than reasoning about the empty function.
    if rgc::Inst::<u32>::try_new(num_insts).is_none()
        || rgc::Block::<u32>::try_new(num_blocks).is_none()
    {
        return Err(RegAllocError::RegicideUnsupported(format!(
            "this function is too large for regicide: {} instructions in {} blocks",
            num_insts, num_blocks,
        )));
    }

    let Prepass {
        vreg_to_val,
        val_class,
        val_to_vreg,
        val_remat,
        operands,
        clobbers,
        insts,
        reuse_targets,
    } = prepass;
    let mut tables = ValTables {
        vreg_to_val: vreg_to_val.repopulate(func.num_vregs(), u32::MAX),
        val_class: val_class.cleared(),
        val_to_vreg: val_to_vreg.cleared(),
        val_remat: val_remat.cleared(),
    };
    let operands = operands.cleared();
    let clobbers = clobbers.cleared();
    let insts = insts.repopulate(
        num_insts,
        InstInfo {
            ops_start: 0,
            num_operands: 0,
            block: Block::invalid(),
        },
    );

    // First walk: carve the instruction index space into blocks and give every
    // defined vreg its `Val`, so that the second walk can translate a use that
    // comes before its definition.
    let mut assigned = 0;
    for b in 0..num_blocks {
        let block = Block::new(b);

        let range = func.block_insns(block);
        if range.len() == 0 {
            // A block always has at least its terminator.
            return Err(RegAllocError::BB(block));
        }
        for inst in range.iter() {
            if inst.index() >= num_insts || insts[inst.index()].block.is_valid() {
                return Err(RegAllocError::BB(block));
            }
            insts[inst.index()].block = block;
            assigned += 1;
        }

        for &vreg in func.block_params(block) {
            // No instruction defines a block parameter, so there is nothing
            // to re-execute.
            define_val(
                &mut tables,
                machine,
                multiple_defs_allowed,
                vreg,
                Inst::invalid(),
                None,
            )?;
        }
        for inst in range.iter() {
            // regicide asks this of the operands after translation, where a
            // `fixed_nonallocatable` operand is a `Use`, so it counts here
            // too.
            let reads_nothing = !func
                .inst_operands(inst)
                .iter()
                .any(|op| op.kind() == OperandKind::Use);
            for op in func.inst_operands(inst) {
                if op.as_fixed_nonallocatable().is_some() {
                    continue;
                }
                if op.kind() == OperandKind::Def {
                    let remat = remat_cost_of_def(func, op.vreg(), reads_nothing);
                    define_val(
                        &mut tables,
                        machine,
                        multiple_defs_allowed,
                        op.vreg(),
                        inst,
                        remat,
                    )?;
                }
            }
        }
    }
    if assigned != num_insts {
        return Err(RegAllocError::RegicideUnsupported(String::from(
            "regicide requires every instruction to lie within some block",
        )));
    }

    // Second walk: translate every operand, and gather every clobber.
    for i in 0..num_insts {
        let inst = Inst::new(i);
        let ops = func.inst_operands(inst);
        let ops_start = operands.len();

        for &op in ops {
            operands.push(translate_operand(machine, vreg_to_val, op, inst)?);
        }
        resolve_reuses(val_class, &mut operands[ops_start..], reuse_targets)?;

        if ops.len() > rgc::MAX_OPERANDS_PER_INST {
            return Err(RegAllocError::TooManyOperands);
        }

        // regicide's clobber set means the same thing, so this is a straight
        // translation.
        let mut clobbered = rgc::RegSet::new();
        for preg in func.inst_clobbers(inst) {
            // A register the allocator does not own holds nothing to
            // destroy, so the clobber says nothing. regicide skips these
            // itself; dropping them here just keeps the set small.
            if !machine.allocatable.contains(preg) {
                continue;
            }

            // A `scratch_by_class` register is allocatable here, precisely
            // so moves can be staged through it, so clobbering it is a real
            // conflict. `singleton` has no class for it and says so; the
            // class is not wanted, and this call is for that refusal alone.
            let _ = machine.singleton(preg)?;

            clobbered.insert(
                machine.reg_of_preg[preg.index()].expect(
                    "every allocatable `PReg` was given a `Reg` when the machine was built",
                ),
            );
        }
        clobbers.push(clobbered);

        insts[i].ops_start = u32::try_from(ops_start).expect("an operand index fits in a u32");
        insts[i].num_operands = u32::try_from(ops.len()).expect("an operand index fits in a u32");
    }

    // Finally, the edges. A block argument is not an operand, so nothing above
    // has looked at one, and regicide asks about every edge.
    for b in 0..num_blocks {
        let block = Block::new(b);
        let last = func.block_insns(block).last();
        for (i, &succ) in func.block_succs(block).iter().enumerate() {
            if succ.index() >= num_blocks {
                return Err(RegAllocError::BB(block));
            }
            let args = func.branch_blockparams(block, last, i);
            if args.len() != func.block_params(succ).len() {
                return Err(RegAllocError::Branch(last));
            }
            for &vreg in args {
                val_of(vreg_to_val, vreg, last)?;
            }
        }
        for &pred in func.block_preds(block) {
            if pred.index() >= num_blocks {
                return Err(RegAllocError::BB(block));
            }
        }
    }

    Ok(())
}

/// The regicide `Inst` with the given index.
fn rgc_inst(index: usize) -> rgc::Inst {
    rgc::Inst::try_new(index).expect("the pre-pass checked that every Inst is representable")
}

/// The regicide `Block` with the given index.
fn rgc_block(index: usize) -> rgc::Block {
    rgc::Block::try_new(index).expect("the pre-pass checked that every Block is representable")
}

/// A [`Function`] and its pre-pass, seen as a `regicide::InputFunction`.
struct Adaptor<'a, F> {
    func: &'a F,
    cfginfo: &'a CFGInfo,
    prepass: &'a Prepass,
}

/// One block of an [`Adaptor`].
struct AdaptorBlock<'a, F> {
    func: &'a F,
    cfginfo: &'a CFGInfo,
    prepass: &'a Prepass,
    block: Block,
}

/// One instruction of an [`Adaptor`].
///
/// The pre-pass has already translated every operand, so this needs nothing
/// from the `Function` itself and is not generic over it.
#[derive(Clone, Copy)]
struct AdaptorInst<'a> {
    prepass: &'a Prepass,
    inst: usize,
}

impl<'a> AdaptorInst<'a> {
    fn ops(&self) -> &'a [rgc::Operand] {
        let info = self.prepass.insts[self.inst];
        let start = info.ops_start as usize;
        &self.prepass.operands[start..start + info.num_operands as usize]
    }
}

/// One value of an [`Adaptor`].
#[derive(Clone, Copy)]
struct AdaptorValue {
    val: rgc::Val,
    class: rgc::RegClass,
    remat_cost: Option<rgc::RematCost>,
}

/// Iterates over an [`Adaptor`]'s values.
struct AdaptorValuesIter<'a> {
    classes: iter::Enumerate<slice::Iter<'a, rgc::RegClass>>,
    remat: &'a [Option<rgc::RematCost>],
}

/// Iterates over an [`Adaptor`]'s blocks.
struct AdaptorBlocksIter<'a, F> {
    func: &'a F,
    cfginfo: &'a CFGInfo,
    prepass: &'a Prepass,
    blocks: Range<usize>,
}

/// Iterates over the instructions of an [`AdaptorBlock`].
struct AdaptorInstsIter<'a> {
    prepass: &'a Prepass,
    insts: Range<usize>,
}

/// Iterates over a slice of vregs as the `Val`s standing for them.
///
/// This serves both a block's parameters and the arguments passed along one of
/// its outgoing edges.
struct ValIter<'a> {
    vregs: slice::Iter<'a, VReg>,
    vreg_to_val: &'a [u32],
}

/// Iterates over a slice of blocks as regicide `Block`s.
struct BlockIter<'a> {
    blocks: slice::Iter<'a, Block>,
}

impl Iterator for AdaptorValuesIter<'_> {
    type Item = (rgc::Val, AdaptorValue);

    fn next(&mut self) -> Option<Self::Item> {
        let (index, &class) = self.classes.next()?;
        let val = rgc::Val::try_new(index).expect("Vals are numbered densely from zero");
        Some((
            val,
            AdaptorValue {
                val,
                class,
                remat_cost: self.remat[index],
            },
        ))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.classes.size_hint()
    }
}

impl<'a, F: Function> Iterator for AdaptorBlocksIter<'a, F> {
    type Item = (rgc::Block, AdaptorBlock<'a, F>);

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.blocks.next()?;
        Some((
            rgc_block(index),
            AdaptorBlock {
                func: self.func,
                cfginfo: self.cfginfo,
                prepass: self.prepass,
                block: Block::new(index),
            },
        ))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.blocks.size_hint()
    }
}

impl<'a> AdaptorInstsIter<'a> {
    fn item(&self, index: usize) -> (rgc::Inst, AdaptorInst<'a>) {
        (
            rgc_inst(index),
            AdaptorInst {
                prepass: self.prepass,
                inst: index,
            },
        )
    }
}

impl<'a> Iterator for AdaptorInstsIter<'a> {
    type Item = (rgc::Inst, AdaptorInst<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.insts.next()?;
        Some(self.item(index))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.insts.size_hint()
    }
}

impl DoubleEndedIterator for AdaptorInstsIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let index = self.insts.next_back()?;
        Some(self.item(index))
    }
}

impl ExactSizeIterator for AdaptorInstsIter<'_> {}

impl Iterator for ValIter<'_> {
    type Item = rgc::Val;

    fn next(&mut self) -> Option<Self::Item> {
        let vreg = self.vregs.next()?;
        let val = self.vreg_to_val[vreg.vreg()];
        Some(rgc::Val::try_new(val as usize).expect("the pre-pass checked every block argument"))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.vregs.size_hint()
    }
}

impl ExactSizeIterator for ValIter<'_> {}

impl Iterator for BlockIter<'_> {
    type Item = rgc::Block;

    fn next(&mut self) -> Option<Self::Item> {
        Some(rgc_block(self.blocks.next()?.index()))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.blocks.size_hint()
    }
}

impl ExactSizeIterator for BlockIter<'_> {}

impl<F: Function> rgc::InputFunction for Adaptor<'_, F> {
    type InputBlock<'b>
        = AdaptorBlock<'b, F>
    where
        Self: 'b;

    type InputInst<'b>
        = AdaptorInst<'b>
    where
        Self: 'b;

    type InputValue<'b>
        = AdaptorValue
    where
        Self: 'b;

    fn len_values(&self) -> usize {
        self.prepass.val_class.len()
    }

    type ValuesIter<'b>
        = AdaptorValuesIter<'b>
    where
        Self: 'b;

    fn values(&self) -> Self::ValuesIter<'_> {
        AdaptorValuesIter {
            classes: self.prepass.val_class.iter().enumerate(),
            remat: &self.prepass.val_remat,
        }
    }

    fn value(&self, val: rgc::Val) -> Self::InputValue<'_> {
        AdaptorValue {
            val,
            class: self.prepass.val_class[val.index()],
            remat_cost: self.prepass.val_remat[val.index()],
        }
    }

    fn len_insts(&self) -> usize {
        self.prepass.insts.len()
    }

    fn inst(&self, inst: rgc::Inst) -> Self::InputInst<'_> {
        AdaptorInst {
            prepass: self.prepass,
            inst: inst.index(),
        }
    }

    fn entry_block(&self) -> rgc::Block {
        rgc_block(self.func.entry_block().index())
    }

    fn len_blocks(&self) -> usize {
        self.func.num_blocks()
    }

    fn block(&self, block: rgc::Block) -> Self::InputBlock<'_> {
        AdaptorBlock {
            func: self.func,
            cfginfo: self.cfginfo,
            prepass: self.prepass,
            block: Block::new(block.index()),
        }
    }

    type BlocksIter<'b>
        = AdaptorBlocksIter<'b, F>
    where
        Self: 'b;

    fn blocks(&self) -> Self::BlocksIter<'_> {
        AdaptorBlocksIter {
            func: self.func,
            cfginfo: self.cfginfo,
            prepass: self.prepass,
            blocks: 0..self.func.num_blocks(),
        }
    }
}

impl rgc::InputValue<'_> for AdaptorValue {
    fn val(&self) -> rgc::Val {
        self.val
    }

    fn remat_cost(&self) -> Option<rgc::RematCost> {
        self.remat_cost
    }

    fn reg_class(&self) -> rgc::RegClass {
        self.class
    }
}

impl<'a> rgc::InputInst<'a> for AdaptorInst<'a> {
    fn inst(&self) -> rgc::Inst {
        rgc_inst(self.inst)
    }

    fn operands_len(&self) -> usize {
        self.prepass.insts[self.inst].num_operands as usize
    }

    type OperandsIter<'b>
        = iter::Copied<slice::Iter<'b, rgc::Operand>>
    where
        Self: 'b;

    fn operands(&self) -> Self::OperandsIter<'_> {
        self.ops().iter().copied()
    }

    fn operand(&self, i: usize) -> rgc::Operand {
        self.ops()[i]
    }

    fn clobbers(&self) -> rgc::RegSet {
        // A 64-byte copy; `RegSet` is a plain bit array.
        self.prepass.clobbers[self.inst].clone()
    }

    fn block(&self) -> rgc::Block {
        rgc_block(self.prepass.insts[self.inst].block.index())
    }
}

impl<'a, F: Function> rgc::InputBlock<'a> for AdaptorBlock<'a, F> {
    type InputInst<'b>
        = AdaptorInst<'b>
    where
        Self: 'b;

    fn block(&self) -> rgc::Block {
        rgc_block(self.block.index())
    }

    fn frequency(&self) -> rgc::BlockFrequency {
        self.func.block_frequency(self.block)
    }

    fn len_insts(&self, _block: rgc::Block) -> usize {
        self.func.block_insns(self.block).len()
    }

    type InstsIter<'b>
        = AdaptorInstsIter<'b>
    where
        Self: 'b;

    fn insts(&self) -> Self::InstsIter<'_> {
        let range = self.func.block_insns(self.block);
        AdaptorInstsIter {
            prepass: self.prepass,
            insts: range.first().index()..range.first().index() + range.len(),
        }
    }

    fn len_params(&self) -> usize {
        self.func.block_params(self.block).len()
    }

    type ParamsIter<'b>
        = ValIter<'b>
    where
        Self: 'b;

    fn params(&self) -> Self::ParamsIter<'_> {
        ValIter {
            vregs: self.func.block_params(self.block).iter(),
            vreg_to_val: &self.prepass.vreg_to_val,
        }
    }

    fn immediate_dominator(&self) -> Option<rgc::Block> {
        // Every block is known to be reachable — the pre-pass checked — so the
        // one block with no immediate dominator is the entry block, and the
        // domtree's sentinel is unambiguous.
        let idom = self.cfginfo.domtree[self.block.index()];
        if idom.is_valid() {
            Some(rgc_block(idom.index()))
        } else {
            None
        }
    }

    fn dominates(&self, other: rgc::Block) -> bool {
        self.cfginfo
            .dominates(self.block, Block::new(other.index()))
    }

    fn len_successors(&self) -> usize {
        self.func.block_succs(self.block).len()
    }

    type SuccessorsIter<'b>
        = BlockIter<'b>
    where
        Self: 'b;

    fn successors(&self) -> Self::SuccessorsIter<'_> {
        BlockIter {
            blocks: self.func.block_succs(self.block).iter(),
        }
    }

    fn len_predecessors(&self) -> usize {
        self.func.block_preds(self.block).len()
    }

    type PredecessorsIter<'b>
        = BlockIter<'b>
    where
        Self: 'b;

    fn predecessors(&self) -> Self::PredecessorsIter<'_> {
        BlockIter {
            blocks: self.func.block_preds(self.block).iter(),
        }
    }

    fn len_successor_block_args(&self, i: usize) -> usize {
        self.branch_blockparams(i).len()
    }

    type SuccessorBlockArgsIter<'b>
        = ValIter<'b>
    where
        Self: 'b;

    fn successor_block_args(&self, successor: usize) -> Self::SuccessorBlockArgsIter<'_> {
        ValIter {
            vregs: self.branch_blockparams(successor).iter(),
            vreg_to_val: &self.prepass.vreg_to_val,
        }
    }
}

impl<'a, F: Function> AdaptorBlock<'a, F> {
    fn branch_blockparams(&self, succ: usize) -> &'a [VReg] {
        let last = self.func.block_insns(self.block).last();
        self.func.branch_blockparams(self.block, last, succ)
    }
}

/// Render an error regicide returned as one of this crate's.
fn map_run_error(error: rgc::Error) -> RegAllocError {
    match error {
        rgc::Error::TooManyLiveRegs => RegAllocError::TooManyLiveRegs,
        rgc::Error::TooManyOperands { .. } => RegAllocError::TooManyOperands,
        error => RegAllocError::Regicide(format!("{}", error)),
    }
}

/// Report that regicide's answer is one this crate's interface cannot carry.
fn shim_error(what: String) -> RegAllocError {
    RegAllocError::RegicideShim(what)
}

/// Translate one of regicide's allocations.
fn translate_alloc(
    machine: &MachineCache,
    spillslot_map: &[u32],
    alloc: rgc::Allocation,
) -> Result<Allocation, RegAllocError> {
    match alloc {
        rgc::Allocation::Reg(rgc::RegAllocation::Reg(reg)) => {
            Ok(Allocation::reg(machine.preg_of_reg[reg.index()]))
        }
        rgc::Allocation::Stack(rgc::StackAllocation::SpillSlot(slot)) => Ok(Allocation::stack(
            SpillSlot::new(spillslot_map[slot.index()] as usize),
        )),
        // Only a value of a register-group class is ever placed in a group, and
        // this shim defines no such class.
        alloc => Err(shim_error(format!(
            "regicide chose {:?}, which this crate's `Allocation` cannot name",
            alloc
        ))),
    }
}

/// Translate one of regicide's moves or rematerializations, wherever it sits.
fn translate_edit(
    machine: &MachineCache,
    prepass: &Prepass,
    spillslot_map: &[u32],
    edit: rgc::EdgeEdit,
) -> Edit {
    let reg = |reg: rgc::Reg| Allocation::reg(machine.preg_of_reg[reg.index()]);
    let slot =
        |slot: rgc::SpillSlot| Allocation::stack(SpillSlot::new(spillslot_map[slot.index()] as _));

    let mv = match edit {
        rgc::EdgeEdit::Move(mv) => mv,
        rgc::EdgeEdit::Remat(rgc::Remat { val, dest }) => {
            // This names the vreg whose defining instruction the client
            // re-executes, so atop a block or on a critical edge it stays the
            // predecessor's argument rather than the parameter it delivers: a
            // parameter has no definition to re-run. The checker handles that
            // renaming.
            return Edit::Remat {
                vreg: prepass.val_to_vreg[val.index()],
                to: reg(dest),
            };
        }
    };

    match mv {
        rgc::Move::RegReg { from, to } => Edit::Move {
            from: reg(from),
            to: reg(to),
        },
        rgc::Move::Spill { from, to } => Edit::Move {
            from: reg(from),
            to: slot(to),
        },
        rgc::Move::Reload { from, to } => Edit::Move {
            from: slot(from),
            to: reg(to),
        },
    }
}

/// Write regicide's answer into `output` in this crate's terms.
fn translate_output<F: Function>(
    func: &F,
    machine: &MachineCache,
    adaptor: &Adaptor<'_, F>,
    result: &rgc::Output<'_>,
    output: &mut crate::Output,
    spillslot_map: &mut Vec<u32>,
    allow_critical_edges: bool,
) -> Result<(), RegAllocError> {
    let num_insts = func.num_insts();

    // The same reset ion does at the top of every run, so that a `Ctx` handed
    // one function after another carries nothing between them.
    output.allocs.preallocate(4 * num_insts);
    output.inst_alloc_offsets.clear();
    output.num_spillslots = 0;
    // regalloc2 tracks where a debug value lives across its live range;
    // regicide has no such notion, so there is nothing to report.
    output.debug_locations.clear();
    output.edits.clear();
    output.critical_edge_edits.clear();
    output.stats = Default::default();

    // Lay regicide's spill slots out in this crate's slot space, by the very
    // arithmetic `ion::Env::allocate_spillslot` uses, so that a client reading
    // `num_spillslots` and `multi_spillslot_named_by_last_slot` sees what it
    // would have from ion.
    let slots = result.spill_slots();
    spillslot_map.clear();
    spillslot_map.resize(slots.len(), u32::MAX);
    let mut offset: u32 = 0;
    for (slot, size) in slots {
        let size = u32::from(size.size());
        debug_assert!(size.is_power_of_two());
        offset = (offset + size - 1) & !(size - 1);
        let named = if func.multi_spillslot_named_by_last_slot() {
            offset + size - 1
        } else {
            offset
        };
        offset += size;
        if named as usize > SpillSlot::MAX {
            return Err(RegAllocError::RegicideUnsupported(format!(
                "this function needs more than {} spill slots",
                SpillSlot::MAX
            )));
        }
        spillslot_map[slot.index()] = named;
    }
    output.num_spillslots = offset as usize;

    // The allocations, one run per instruction.
    for i in 0..num_insts {
        output
            .inst_alloc_offsets
            .push(u32::try_from(output.allocs.len()).expect("an alloc index fits in a u32"));
        for &alloc in result.inst_allocs(rgc_inst(i)) {
            let alloc = translate_alloc(machine, spillslot_map, alloc)?;
            output.allocs.push(alloc);
        }
    }

    // The edits. regicide streams each block's edits interleaved with its
    // instructions; this crate instead anchors an edit to a program point, and
    // the point of the gap an edit sits in is `before` the instruction that
    // follows it. So edits are pushed as they arrive and their point is filled
    // in once the next instruction names it.
    let mut pending = 0usize;
    let mut last: Option<ProgPoint> = None;
    let mut sorted = true;
    for b in 0..func.num_blocks() {
        for item in result.block_insts_and_edits(adaptor, rgc_block(b)) {
            match item {
                rgc::InstOrEdit::Inst(inst) => {
                    if pending > 0 {
                        let at = ProgPoint::before(Inst::new(inst.index()));
                        let first = output.edits.len() - pending;
                        for edit in &mut output.edits[first..] {
                            edit.0 = at;
                        }
                        pending = 0;
                        if last.map_or(false, |previous| at < previous) {
                            sorted = false;
                        }
                        last = Some(at);
                    }
                }
                rgc::InstOrEdit::Edit(rgc::Edit::CriticalEdge(rgc::CriticalEdgeEdit {
                    target,
                    succ,
                    edit,
                })) => {
                    // `CFGInfo::init` rejects every function regicide would
                    // want to split an edge of, unless the client asked to
                    // split them itself.
                    if !allow_critical_edges {
                        return Err(shim_error(format!(
                            "regicide put {:?} on a critical edge, which the client \
                             did not allow",
                            edit
                        )));
                    }
                    let block = Block::new(b);
                    debug_assert_eq!(func.block_succs(block)[succ], Block::new(target.index()));
                    output.critical_edge_edits.push(crate::CriticalEdgeEdit {
                        block,
                        succ_idx: u32::try_from(succ).expect("regicide bounds successors"),
                        edit: translate_edit(machine, adaptor.prepass, spillslot_map, edit),
                    });
                }
                rgc::InstOrEdit::Edit(edit) => {
                    let edit = match edit {
                        rgc::Edit::Move(mv) => rgc::EdgeEdit::Move(mv),
                        rgc::Edit::Remat(remat) => rgc::EdgeEdit::Remat(remat),
                        rgc::Edit::CriticalEdge(_) => unreachable!("handled above"),
                    };
                    let edit = translate_edit(machine, adaptor.prepass, spillslot_map, edit);
                    output.edits.push((ProgPoint::before(Inst::new(0)), edit));
                    pending += 1;
                }
            }
        }

        // regicide never places a move or a remat after a block's terminator,
        // so the stream always ends with an instruction to anchor to. Only
        // critical-edge edits come after it, and those are not pending.
        if pending != 0 {
            return Err(shim_error(String::from(
                "regicide placed an edit after a block's terminator, where this \
                 crate's `Output` cannot carry it",
            )));
        }
    }

    // `Output::block_insts_and_edits` binary-searches the edits, so they have
    // to be sorted by program point overall. Blocks are walked in index order,
    // which is usually instruction order too, and then they already are; a
    // function whose blocks are laid out in some other order needs the sort.
    // It has to be a stable one, so that the moves of a parallel move at one
    // point keep the order regicide sequentialized them into.
    if !sorted {
        output.edits.sort_by_key(|(at, _)| *at);
    }

    Ok(())
}

/// Allocate registers for `func` with regicide, writing the result into
/// `ctx.output`.
///
/// Mirrors [`ion::run`][crate::ion::run]: the caller owns the output buffer, so
/// a client that allocates for many functions pays for it once.
pub(crate) fn run<F: Function>(
    func: &F,
    env: &MachineEnv,
    options: &RegallocOptions,
    ctx: &mut Ctx,
) -> Result<(), RegAllocError> {
    // regicide's `Output` borrows its `Context` for as long as it lives, so the
    // pieces of `Ctx` the translation reads and writes have to be held as
    // separate borrows rather than through `ctx` itself.
    let Ctx {
        cfginfo,
        cfginfo_ctx,
        output,
        regicide:
            RegicideCtx {
                context,
                machine,
                prepass,
                spillslot_map,
            },
        ..
    } = ctx;

    let allow_critical_edges = options.allow_critical_edges && supports_critical_edges();

    // This both builds the dominator tree regicide asks about and, unless the
    // client splits critical edges itself, performs the critical-edge and
    // branch-argument checks that make `Edit::CriticalEdge` unreachable.
    cfginfo.init(func, cfginfo_ctx, allow_critical_edges)?;
    let cfginfo = &*cfginfo;

    let key = MachineKey::new(func, env);
    if machine.as_ref().map_or(true, |cached| cached.key != key) {
        *machine = Some(MachineCache::build(key, env)?);
    }
    let machine = machine.as_ref().expect("just built");

    build_prepass(prepass, func, cfginfo, machine)?;
    let prepass = &*prepass;

    let adaptor = Adaptor {
        func,
        cfginfo,
        prepass,
    };
    let input_options = rgc::InputOptions {
        validate_input: options.validate_ssa || cfg!(debug_assertions),
        // One knob here drives both of regicide's, which is all
        // `RegallocOptions` can say; the rest keep regicide's defaults.
        redundant_move_elimination_level: options.move_elimination_level,
        dead_move_elimination_level: options.move_elimination_level,
        ..rgc::InputOptions::new()
    };
    let input = rgc::Input {
        options: &input_options,
        machine: &machine.machine,
        func: &adaptor,
    };

    // `regicide::run` asserts this, and a `Context` kept across runs is not
    // reset when the run that used it last finished with it.
    context.reset();

    // Captured before the run, so that an allocation that never returns still
    // leaves its input behind.
    #[cfg(feature = "regicide-dump")]
    {
        dump::maybe_dump(&input, func);
        if dump::dump_only() {
            *output = crate::fastalloc::run(func, env, options.verbose_log, options.validate_ssa)?;
            return Ok(());
        }
    }

    let result = rgc::run(context, &input).map_err(map_run_error)?;

    translate_output(
        func,
        machine,
        &adaptor,
        &result,
        output,
        spillslot_map,
        allow_critical_edges,
    )
}

/// Whether a run will really be regicide's, and so can report critical-edge
/// edits; see `Algorithm::supports_critical_edges`.
pub(crate) fn supports_critical_edges() -> bool {
    #[cfg(feature = "regicide-dump")]
    if dump::dump_only() {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    //! Tests for the shim paths that fuzzing cannot reach.
    //!
    //! `fuzzing::func` only ever generates `Any`, `Reg`, `FixedReg` and
    //! `Reuse` constraints, and its `machine_env` names no scratch register,
    //! so `OperandConstraint::Stack`, `OperandConstraint::Limit` and the
    //! class-less scratch register have no other coverage.

    use super::*;
    use crate::checker::Checker;
    use crate::{Algorithm, InstRange, Output};
    use alloc::vec;

    /// A single straight-line block ending in a return.
    struct Probe {
        insts: Vec<Vec<Operand>>,
        num_vregs: usize,
        /// The registers this function's instructions clobber.
        clobbers: PRegSet,
        /// Which instruction clobbers them, or `None` for all of them.
        clobbers_at: Option<usize>,
        /// Each class's spill-slot size; `None` for a class that panics when
        /// asked.
        spillslot_size: [Option<usize>; NUM_CLASSES],
        remat: Vec<(VReg, rgc::RematCost)>,
    }

    impl Probe {
        fn new(insts: Vec<Vec<Operand>>) -> Self {
            let num_vregs = insts
                .iter()
                .flatten()
                .map(|op| op.vreg().vreg() + 1)
                .max()
                .unwrap_or(0);
            Probe {
                insts,
                num_vregs,
                clobbers: PRegSet::empty(),
                clobbers_at: None,
                spillslot_size: [Some(1); NUM_CLASSES],
                remat: vec![],
            }
        }

        /// Make every instruction clobber `preg`.
        fn clobbering(mut self, preg: PReg) -> Self {
            self.clobbers.add(preg);
            self
        }

        /// Make only instruction `inst` clobber, and have it clobber `pregs`.
        fn clobbering_only_at(mut self, inst: usize, pregs: PRegSet) -> Self {
            self.clobbers = pregs;
            self.clobbers_at = Some(inst);
            self
        }

        fn rematerializable(mut self, vregs: &[VReg], cost: rgc::RematCost) -> Self {
            self.remat.extend(vregs.iter().map(|&v| (v, cost)));
            self
        }

        /// Make `class` panic when asked its spill-slot size, as Cranelift's
        /// backends do for a class they grant no register for.
        fn with_unanswerable_spillslot_size(mut self, class: RegClass) -> Self {
            self.spillslot_size[class as usize] = None;
            self
        }
    }

    impl Function for Probe {
        fn num_insts(&self) -> usize {
            self.insts.len()
        }
        fn num_blocks(&self) -> usize {
            1
        }
        fn entry_block(&self) -> Block {
            Block::new(0)
        }
        fn block_insns(&self, _: Block) -> InstRange {
            InstRange::new(Inst::new(0), Inst::new(self.insts.len()))
        }
        fn block_succs(&self, _: Block) -> &[Block] {
            &[]
        }
        fn block_preds(&self, _: Block) -> &[Block] {
            &[]
        }
        fn block_params(&self, _: Block) -> &[VReg] {
            &[]
        }
        fn is_ret(&self, insn: Inst) -> bool {
            insn.index() == self.insts.len() - 1
        }
        fn is_branch(&self, _: Inst) -> bool {
            false
        }
        fn branch_blockparams(&self, _: Block, _: Inst, _: usize) -> &[VReg] {
            &[]
        }
        fn inst_operands(&self, insn: Inst) -> &[Operand] {
            &self.insts[insn.index()]
        }
        fn inst_clobbers(&self, insn: Inst) -> PRegSet {
            match self.clobbers_at {
                Some(i) if i != insn.index() => PRegSet::empty(),
                _ => self.clobbers,
            }
        }
        fn num_vregs(&self) -> usize {
            self.num_vregs
        }
        fn is_rematerializable(&self, vreg: VReg) -> Option<rgc::RematCost> {
            self.remat
                .iter()
                .find(|(v, _)| *v == vreg)
                .map(|(_, cost)| *cost)
        }
        fn spillslot_size(&self, class: RegClass) -> usize {
            self.spillslot_size[class as usize].unwrap_or_else(|| {
                panic!("this target has no spill-slot size for {:?} to give", class)
            })
        }
    }

    /// A function of several blocks, for the tests that need more than one.
    ///
    /// block0 falls to every other block, each of which returns, so there is
    /// no critical edge and no block parameter: values flow by dominance.
    struct Branching {
        /// Each block's instructions, as operand lists. The empty last one in
        /// each block is its terminator.
        blocks: Vec<Vec<Vec<Operand>>>,
        /// Which blocks are cold.
        cold: Vec<usize>,
        num_vregs: usize,
        /// Held rather than built on demand because the trait hands out
        /// borrows that outlive the call.
        succs_of_entry: Vec<Block>,
        entry_only: Vec<Block>,
    }

    impl Branching {
        fn new(blocks: Vec<Vec<Vec<Operand>>>, cold: Vec<usize>, num_vregs: usize) -> Self {
            let succs_of_entry = (1..blocks.len()).map(Block::new).collect();
            Branching {
                blocks,
                cold,
                num_vregs,
                succs_of_entry,
                entry_only: vec![Block::new(0)],
            }
        }

        /// The index of `block`'s first instruction.
        fn first_inst_of(&self, block: usize) -> usize {
            self.blocks[..block].iter().map(|b| b.len()).sum()
        }

        fn inst_at(&self, inst: usize) -> (usize, usize) {
            let mut start = 0;
            for (b, insts) in self.blocks.iter().enumerate() {
                if inst < start + insts.len() {
                    return (b, inst - start);
                }
                start += insts.len();
            }
            panic!("inst{} is past the end of the function", inst)
        }
    }

    impl Function for Branching {
        fn num_insts(&self) -> usize {
            self.blocks.iter().map(|b| b.len()).sum()
        }
        fn num_blocks(&self) -> usize {
            self.blocks.len()
        }
        fn entry_block(&self) -> Block {
            Block::new(0)
        }
        fn block_insns(&self, block: Block) -> InstRange {
            let start = self.first_inst_of(block.index());
            InstRange::new(
                Inst::new(start),
                Inst::new(start + self.blocks[block.index()].len()),
            )
        }
        fn block_succs(&self, block: Block) -> &[Block] {
            if block.index() == 0 {
                &self.succs_of_entry
            } else {
                &[]
            }
        }
        fn block_preds(&self, block: Block) -> &[Block] {
            if block.index() == 0 {
                &[]
            } else {
                &self.entry_only
            }
        }
        fn block_params(&self, _: Block) -> &[VReg] {
            &[]
        }
        fn block_frequency(&self, block: Block) -> rgc::BlockFrequency {
            if self.cold.contains(&block.index()) {
                rgc::BlockFrequency::COLD
            } else {
                rgc::BlockFrequency::NORMAL
            }
        }
        fn is_ret(&self, insn: Inst) -> bool {
            let (block, i) = self.inst_at(insn.index());
            block != 0 && i == self.blocks[block].len() - 1
        }
        fn is_branch(&self, insn: Inst) -> bool {
            let (block, i) = self.inst_at(insn.index());
            block == 0 && i == self.blocks[block].len() - 1
        }
        fn branch_blockparams(&self, _: Block, _: Inst, _: usize) -> &[VReg] {
            &[]
        }
        fn inst_operands(&self, insn: Inst) -> &[Operand] {
            let (block, i) = self.inst_at(insn.index());
            &self.blocks[block][i]
        }
        fn inst_clobbers(&self, _: Inst) -> PRegSet {
            PRegSet::empty()
        }
        fn num_vregs(&self) -> usize {
            self.num_vregs
        }
        fn spillslot_size(&self, _: RegClass) -> usize {
            1
        }
    }

    /// The environment `fuzzing::func` uses, but with `hw_enc` 32 of each
    /// class held back so a caller can make it a scratch register.
    fn env_with_scratch(scratch: [Option<PReg>; NUM_CLASSES]) -> MachineEnv {
        fn regs(r: Range<usize>, c: RegClass) -> PRegSet {
            r.map(|i| PReg::new(i, c)).collect()
        }
        MachineEnv {
            preferred_regs_by_class: [
                regs(0..24, RegClass::Int),
                regs(0..24, RegClass::Float),
                regs(0..24, RegClass::Vector),
            ],
            non_preferred_regs_by_class: [
                regs(24..32, RegClass::Int),
                regs(24..32, RegClass::Float),
                regs(24..32, RegClass::Vector),
            ],
            scratch_by_class: scratch,
            fixed_stack_slots: vec![],
        }
    }

    /// Allocate with regicide and run this crate's checker over the result.
    fn alloc(func: &Probe, env: &MachineEnv) -> Output {
        let options = RegallocOptions {
            algorithm: Algorithm::Regicide,
            validate_ssa: true,
            ..RegallocOptions::default()
        };
        let output = crate::run(func, env, &options).expect("regicide should allocate this");
        let mut checker = Checker::new(func, env);
        checker.prepare(&output);
        checker.run().expect("checker should accept this");
        output
    }

    #[test]
    fn stack_constraint() {
        let v0 = VReg::new(0, RegClass::Int);
        let func = Probe::new(vec![
            vec![Operand::new(
                v0,
                OperandConstraint::Reg,
                OperandKind::Def,
                OperandPos::Late,
            )],
            vec![Operand::new(
                v0,
                OperandConstraint::Stack,
                OperandKind::Use,
                OperandPos::Early,
            )],
            vec![],
        ]);
        let env = env_with_scratch([None; NUM_CLASSES]);
        let output = alloc(&func, &env);

        let use_alloc = output.inst_allocs(Inst::new(1))[0];
        assert!(
            use_alloc.is_stack(),
            "a `Stack` operand must land on the stack, got {:?}",
            use_alloc,
        );
    }

    #[test]
    fn limit_constraint() {
        // Half the preferred registers: a real class, and not the base class,
        // so `MachineCache::limit_class` has to have built one for it.
        const LIMIT: usize = 16;

        let v0 = VReg::new(0, RegClass::Int);
        let func = Probe::new(vec![
            vec![Operand::new(
                v0,
                OperandConstraint::Limit(LIMIT),
                OperandKind::Def,
                OperandPos::Late,
            )],
            vec![Operand::new(
                v0,
                OperandConstraint::Limit(LIMIT),
                OperandKind::Use,
                OperandPos::Early,
            )],
            vec![],
        ]);
        let env = env_with_scratch([None; NUM_CLASSES]);
        let output = alloc(&func, &env);

        for inst in [0, 1] {
            let alloc = output.inst_allocs(Inst::new(inst))[0];
            let preg = alloc
                .as_reg()
                .expect("a `Limit` operand must get a register");
            assert!(
                preg.hw_enc() < LIMIT,
                "a `Limit({})` operand must get a register below {}, got {:?}",
                LIMIT,
                LIMIT,
                preg,
            );
        }
    }

    #[test]
    fn fixed_stack_slot_is_refused_not_miscompiled() {
        // `fuzzing::regicide` caps fixed registers at the allocatable range,
        // so nothing else now exercises this rejection. regicide defines a
        // non-allocatable register without units and so cannot hold a value
        // in one; the shim must say so rather than allocate something wrong.
        let v0 = VReg::new(0, RegClass::Int);
        let slot = PReg::new(40, RegClass::Int);
        let func = Probe::new(vec![
            vec![Operand::new(
                v0,
                OperandConstraint::FixedReg(slot),
                OperandKind::Def,
                OperandPos::Late,
            )],
            vec![Operand::new(
                v0,
                OperandConstraint::Reg,
                OperandKind::Use,
                OperandPos::Early,
            )],
            vec![],
        ]);
        let mut env = env_with_scratch([None; NUM_CLASSES]);
        env.fixed_stack_slots = vec![slot];

        let options = RegallocOptions {
            algorithm: Algorithm::Regicide,
            validate_ssa: true,
            ..RegallocOptions::default()
        };
        match crate::run(&func, &env, &options) {
            Err(RegAllocError::RegicideUnsupported(_)) => {}
            other => panic!(
                "a value pinned to a fixed stack slot must be refused, got {:?}",
                other.map(|o| o.num_spillslots),
            ),
        }
    }

    /// A one-instruction function that returns, and does nothing else.
    fn trivial() -> Probe {
        Probe::new(vec![vec![]])
    }

    #[test]
    fn an_ungranted_class_is_never_asked_its_spillslot_size() {
        // aarch64, x64 and s390x all answer `spillslot_size(RegClass::Vector)`
        // with `unreachable!()`, so the shim must not ask.
        let func = trivial().with_unanswerable_spillslot_size(RegClass::Vector);

        let mut env = env_with_scratch([None; NUM_CLASSES]);
        env.preferred_regs_by_class[RegClass::Vector as usize] = PRegSet::empty();
        env.non_preferred_regs_by_class[RegClass::Vector as usize] = PRegSet::empty();

        // The panic, if the shim asks, is the failure.
        alloc(&func, &env);
    }

    #[test]
    fn a_clobber_of_a_register_the_allocator_does_not_own_is_a_no_op() {
        // aarch64 withholds `x16`/`x17` yet lists them among every call's
        // clobbers, so every call would otherwise be unallocatable.
        let v0 = VReg::new(0, RegClass::Int);
        let env = env_with_scratch([None; NUM_CLASSES]);

        let unowned = PReg::new(40, RegClass::Int);
        assert!(
            !env.preferred_regs_by_class[RegClass::Int as usize].contains(unowned)
                && !env.non_preferred_regs_by_class[RegClass::Int as usize].contains(unowned),
            "the test only means something if the environment withholds this register",
        );

        let func = Probe::new(vec![
            vec![Operand::new(
                v0,
                OperandConstraint::Reg,
                OperandKind::Def,
                OperandPos::Late,
            )],
            vec![Operand::new(
                v0,
                OperandConstraint::Reg,
                OperandKind::Use,
                OperandPos::Early,
            )],
            vec![],
        ])
        .clobbering(unowned);

        let output = alloc(&func, &env);

        // Nothing may be placed in the register on its account either.
        for inst in 0..func.num_insts() {
            for alloc in output.inst_allocs(Inst::new(inst)) {
                assert_ne!(alloc.as_reg(), Some(unowned));
            }
        }
    }

    #[test]
    fn an_instruction_clobbering_every_register_still_allocates() {
        // A call on aarch64 clobbers every Int register the allocator owns.
        // Each reserves only a late half, which an early read does not want.
        let env = env_with_scratch([None; NUM_CLASSES]);
        let granted: PRegSet = env.preferred_regs_by_class[RegClass::Int as usize]
            .into_iter()
            .chain(env.non_preferred_regs_by_class[RegClass::Int as usize])
            .collect();
        assert_eq!(
            granted.into_iter().count(),
            32,
            "the test only means something if the environment grants registers",
        );

        let v0 = VReg::new(0, RegClass::Int);
        let func = Probe::new(vec![
            vec![Operand::new(
                v0,
                OperandConstraint::Reg,
                OperandKind::Def,
                OperandPos::Late,
            )],
            // The clobbering instruction, reading the value in the early
            // half the clobbers leave alone. The value dies here.
            vec![Operand::new(
                v0,
                OperandConstraint::Reg,
                OperandKind::Use,
                OperandPos::Early,
            )],
            vec![],
        ])
        .clobbering_only_at(1, granted);

        let output = alloc(&func, &env);

        let read = output.inst_allocs(Inst::new(1))[0];
        assert!(
            read.as_reg().is_some_and(|preg| granted.contains(preg)),
            "the early read must get one of the clobbered registers, got {:?}",
            read,
        );
    }

    #[test]
    fn a_clobber_of_the_scratch_register_is_refused_not_ignored() {
        // The other side of the previous test: the scratch register is
        // allocatable, so clobbering it is a real conflict, and the shim must
        // say so rather than drop the clobber.
        let scratch = PReg::new(32, RegClass::Int);
        let func = trivial().clobbering(scratch);
        let env = env_with_scratch([Some(scratch), None, None]);

        let options = RegallocOptions {
            algorithm: Algorithm::Regicide,
            validate_ssa: true,
            ..RegallocOptions::default()
        };
        match crate::run(&func, &env, &options) {
            Err(RegAllocError::RegicideUnsupported(_)) => {}
            other => panic!(
                "a clobber of the scratch register must be refused, got {:?}",
                other.map(|o| o.num_spillslots),
            ),
        }
    }

    #[test]
    fn scratch_register_holds_nothing() {
        // More values live at once than there are registers, so the allocator
        // has to spill and reload; the scratch register is the only one left
        // to stage a stack-to-stack move through, and it must still never be
        // where a value lives.
        const NUM_REGS: usize = 32;
        const NUM_VALS: usize = NUM_REGS + 8;
        let scratch = PReg::new(NUM_REGS, RegClass::Int);
        let env = env_with_scratch([Some(scratch), None, None]);

        let vregs: Vec<VReg> = (0..NUM_VALS).map(|i| VReg::new(i, RegClass::Int)).collect();

        // Define every value, then read each one back in its own instruction,
        // so they are all live across the whole body but no single
        // instruction asks for more registers than exist.
        let mut insts: Vec<Vec<Operand>> = vregs
            .iter()
            .map(|&v| {
                vec![Operand::new(
                    v,
                    OperandConstraint::Reg,
                    OperandKind::Def,
                    OperandPos::Late,
                )]
            })
            .collect();
        insts.extend(vregs.iter().rev().map(|&v| {
            vec![Operand::new(
                v,
                OperandConstraint::Reg,
                OperandKind::Use,
                OperandPos::Early,
            )]
        }));
        insts.push(vec![]);

        let func = Probe::new(insts);
        let output = alloc(&func, &env);

        // Otherwise the test proves nothing: without a spill there is nothing
        // for a scratch register to stage.
        assert!(
            output.num_spillslots > 0,
            "this shape was supposed to outrun the register file",
        );

        for inst in 0..func.num_insts() {
            for alloc in output.inst_allocs(Inst::new(inst)) {
                assert_ne!(
                    alloc.as_reg(),
                    Some(scratch),
                    "the scratch register must never hold a value, but inst{} puts one there",
                    inst,
                );
            }
        }
    }
    /// A value the client says can be recomputed is, rather than being
    /// spilled and reloaded, once the register file runs out.
    #[test]
    fn a_rematerializable_value_is_recomputed_rather_than_reloaded() {
        let env = env_with_scratch([None; NUM_CLASSES]);
        let def = |v| {
            vec![Operand::new(
                v,
                OperandConstraint::Reg,
                OperandKind::Def,
                OperandPos::Late,
            )]
        };
        let use_ = |v| {
            vec![Operand::new(
                v,
                OperandConstraint::Reg,
                OperandKind::Use,
                OperandPos::Early,
            )]
        };

        let remat = VReg::new(0, RegClass::Int);
        let mut insts = vec![def(remat)];

        // 31 values live at once, against the 32 registers granted, leaves
        // `remat` nowhere of its own.
        let busy: Vec<VReg> = (1..32).map(|i| VReg::new(i, RegClass::Int)).collect();
        insts.extend(busy.iter().copied().map(def));
        insts.extend(busy.iter().copied().map(use_));

        insts.push(use_(remat));
        insts.push(vec![]);

        let func = Probe::new(insts).rematerializable(&[remat], rgc::RematCost::AlwaysRemat);
        let output = alloc(&func, &env);

        let remats: Vec<_> = output
            .edits
            .iter()
            .filter_map(|(_, edit)| match edit {
                Edit::Remat { vreg, to } => Some((*vreg, *to)),
                Edit::Move { .. } => None,
            })
            .collect();
        assert!(
            !remats.is_empty(),
            "nothing was rematerialized, so `is_rematerializable` bought \
             nothing here; edits were {:?}",
            output.edits,
        );
        for (vreg, to) in &remats {
            assert_eq!(
                *vreg, remat,
                "only the value this function called rematerializable may be \
                 rematerialized",
            );
            assert!(
                to.is_reg(),
                "a rematerialization writes a register, but this one wrote {:?}",
                to,
            );
        }

        // The point of it all.
        assert_eq!(
            output.num_spillslots, 0,
            "a value that can be recomputed should not need a spill slot",
        );
    }

    /// Allocate a value read in two different fixed registers, so it must be
    /// copied or recomputed, and return its remats and register moves.
    fn remats_and_moves_of_a_value_in_two_regs(
        cost: rgc::RematCost,
    ) -> (Vec<(VReg, Allocation)>, Vec<(Allocation, Allocation)>) {
        let env = env_with_scratch([None; NUM_CLASSES]);
        let v = VReg::new(0, RegClass::Int);
        let use_in = |hw_enc| {
            vec![Operand::new(
                v,
                OperandConstraint::FixedReg(PReg::new(hw_enc, RegClass::Int)),
                OperandKind::Use,
                OperandPos::Early,
            )]
        };
        let insts = vec![
            vec![Operand::new(
                v,
                OperandConstraint::Reg,
                OperandKind::Def,
                OperandPos::Late,
            )],
            use_in(0),
            use_in(1),
            vec![],
        ];

        let func = Probe::new(insts).rematerializable(&[v], cost);
        let output = alloc(&func, &env);

        let mut remats = vec![];
        let mut moves = vec![];
        for (_, edit) in &output.edits {
            match *edit {
                Edit::Remat { vreg, to } => remats.push((vreg, to)),
                Edit::Move { from, to } if from.is_reg() && to.is_reg() => moves.push((from, to)),
                Edit::Move { .. } => {}
            }
        }
        (remats, moves)
    }

    /// A value whose recomputation costs no more than a move is recomputed
    /// into each register that needs it.
    #[test]
    fn an_always_remat_value_is_recomputed_rather_than_moved() {
        let (remats, moves) = remats_and_moves_of_a_value_in_two_regs(rgc::RematCost::AlwaysRemat);
        assert!(
            !remats.is_empty(),
            "nothing was rematerialized; moves were {:?}",
            moves,
        );
        assert!(
            moves.is_empty(),
            "an `AlwaysRemat` value should be recomputed, not moved: {:?}",
            moves,
        );
    }

    /// A value only cheaper to recompute than to reload is moved between
    /// registers instead.
    #[test]
    fn a_cheaper_than_reload_value_is_moved_rather_than_recomputed() {
        let (remats, moves) =
            remats_and_moves_of_a_value_in_two_regs(rgc::RematCost::CheaperThanReload);
        assert!(
            remats.is_empty(),
            "a `CheaperThanReload` value already in a register should be \
             moved, not recomputed: {:?}",
            remats,
        );
        assert!(!moves.is_empty(), "the value was never moved");
    }

    /// A value only a cold block reads loses the register to one a normal
    /// block reads.
    ///
    /// The reads are `Any` rather than `Reg` so a spilled value is read from
    /// the stack; a `Reg` read would be reloaded first and the two cases would
    /// look alike.
    #[test]
    fn a_cold_block_loses_its_register_to_a_warm_one() {
        // One `Int` register, so the two values cannot both stay in one.
        let env = MachineEnv {
            preferred_regs_by_class: [
                core::iter::once(PReg::new(0, RegClass::Int)).collect(),
                PRegSet::empty(),
                PRegSet::empty(),
            ],
            non_preferred_regs_by_class: [PRegSet::empty(); NUM_CLASSES],
            scratch_by_class: [None; NUM_CLASSES],
            fixed_stack_slots: vec![],
        };

        let first = VReg::new(0, RegClass::Int);
        let second = VReg::new(1, RegClass::Int);
        let def = |v| {
            vec![Operand::new(
                v,
                OperandConstraint::Reg,
                OperandKind::Def,
                OperandPos::Late,
            )]
        };
        let read = |v| {
            vec![Operand::new(
                v,
                OperandConstraint::Any,
                OperandKind::Use,
                OperandPos::Early,
            )]
        };

        // Each successor reads both values, so both are live throughout and
        // one must live on the stack, but reads one far more than the other.
        // Which lopsided block is cold is all that differs below.
        let reads = 8;
        let allocs = |cold: Vec<usize>| {
            // Both values live here, but one is plainly worth more.
            let lopsided = |heavy: VReg, light: VReg| -> Vec<Vec<Operand>> {
                (0..reads)
                    .map(|_| read(heavy))
                    .chain(core::iter::once(read(light)))
                    .chain(core::iter::once(vec![]))
                    .collect()
            };
            let func = Branching::new(
                vec![
                    vec![def(first), def(second), vec![]],
                    lopsided(first, second),
                    lopsided(second, first),
                ],
                cold,
                2,
            );
            let options = RegallocOptions {
                algorithm: Algorithm::Regicide,
                validate_ssa: true,
                ..RegallocOptions::default()
            };
            let output = crate::run(&func, &env, &options).expect("should allocate");
            let mut checker = Checker::new(&func, &env);
            checker.prepare(&output);
            checker.run().expect("checker should accept this");
            // How many of each block's reads are served from the stack.
            let on_stack = |block: usize| {
                let start = func.first_inst_of(block);
                (0..reads)
                    .filter(|i| output.inst_allocs(Inst::new(start + i))[0].is_stack())
                    .count()
            };
            (on_stack(1), on_stack(2))
        };

        // The control: one register cannot serve both values.
        let (_, baseline_second) = allocs(vec![]);
        assert!(
            baseline_second > 0,
            "this shape was supposed to force a spill even before any block \
             was called cold",
        );

        // Now call that same block cold. Nothing else changes, so any
        // difference is the frequency being heard.
        let (_, second_when_cold) = allocs(vec![2]);
        assert!(
            second_when_cold < baseline_second,
            "once block2 is cold its value should stop being worth the \
             register, but {} of its reads came from the stack either way",
            baseline_second,
        );
    }

    /// A function of any shape, for the tests that need block parameters or
    /// critical edges.
    struct Cfg {
        /// Each block's instructions, as operand lists; the last one in each
        /// block is its terminator.
        insts: Vec<Vec<Vec<Operand>>>,
        params: Vec<Vec<VReg>>,
        succs: Vec<Vec<Block>>,
        /// The arguments each block passes along each of its edges.
        args: Vec<Vec<Vec<VReg>>>,
        preds: Vec<Vec<Block>>,
        num_vregs: usize,
    }

    /// One block of a `Cfg`: its parameters, its instructions, and each edge
    /// it leaves by with the arguments passed along it.
    type CfgBlock = (Vec<VReg>, Vec<Vec<Operand>>, Vec<(usize, Vec<VReg>)>);

    impl Cfg {
        fn new(blocks: Vec<CfgBlock>) -> Self {
            let mut preds = vec![vec![]; blocks.len()];
            let mut num_vregs = 0;
            let mut cfg = Cfg {
                insts: vec![],
                params: vec![],
                succs: vec![],
                args: vec![],
                preds: vec![],
                num_vregs: 0,
            };
            for (b, (params, insts, edges)) in blocks.into_iter().enumerate() {
                let vregs = params
                    .iter()
                    .copied()
                    .chain(insts.iter().flatten().map(|op| op.vreg()))
                    .chain(edges.iter().flat_map(|(_, args)| args.iter().copied()));
                for vreg in vregs {
                    num_vregs = num_vregs.max(vreg.vreg() + 1);
                }
                for (succ, _) in &edges {
                    preds[*succ].push(Block::new(b));
                }
                cfg.params.push(params);
                cfg.insts.push(insts);
                cfg.succs
                    .push(edges.iter().map(|(succ, _)| Block::new(*succ)).collect());
                cfg.args
                    .push(edges.into_iter().map(|(_, args)| args).collect());
            }
            cfg.preds = preds;
            cfg.num_vregs = num_vregs;
            cfg
        }

        fn first_inst_of(&self, block: usize) -> usize {
            self.insts[..block].iter().map(|b| b.len()).sum()
        }

        fn inst_at(&self, inst: usize) -> (usize, usize) {
            let mut start = 0;
            for (b, insts) in self.insts.iter().enumerate() {
                if inst < start + insts.len() {
                    return (b, inst - start);
                }
                start += insts.len();
            }
            panic!("inst{} is past the end of the function", inst)
        }

        fn is_terminator(&self, insn: Inst) -> Option<usize> {
            let (block, i) = self.inst_at(insn.index());
            (i == self.insts[block].len() - 1).then_some(block)
        }
    }

    impl Function for Cfg {
        fn num_insts(&self) -> usize {
            self.insts.iter().map(|b| b.len()).sum()
        }
        fn num_blocks(&self) -> usize {
            self.insts.len()
        }
        fn entry_block(&self) -> Block {
            Block::new(0)
        }
        fn block_insns(&self, block: Block) -> InstRange {
            let start = self.first_inst_of(block.index());
            InstRange::new(
                Inst::new(start),
                Inst::new(start + self.insts[block.index()].len()),
            )
        }
        fn block_succs(&self, block: Block) -> &[Block] {
            &self.succs[block.index()]
        }
        fn block_preds(&self, block: Block) -> &[Block] {
            &self.preds[block.index()]
        }
        fn block_params(&self, block: Block) -> &[VReg] {
            &self.params[block.index()]
        }
        fn is_ret(&self, insn: Inst) -> bool {
            self.is_terminator(insn)
                .map_or(false, |b| self.succs[b].is_empty())
        }
        fn is_branch(&self, insn: Inst) -> bool {
            self.is_terminator(insn)
                .map_or(false, |b| !self.succs[b].is_empty())
        }
        fn branch_blockparams(&self, block: Block, _: Inst, succ_idx: usize) -> &[VReg] {
            &self.args[block.index()][succ_idx]
        }
        fn inst_operands(&self, insn: Inst) -> &[Operand] {
            let (block, i) = self.inst_at(insn.index());
            &self.insts[block][i]
        }
        fn inst_clobbers(&self, _: Inst) -> PRegSet {
            PRegSet::empty()
        }
        fn num_vregs(&self) -> usize {
            self.num_vregs
        }
        fn spillslot_size(&self, _: RegClass) -> usize {
            1
        }
    }

    /// Allocate `func` with regicide, letting it keep its critical edges, and
    /// run this crate's checker over the result.
    fn alloc_with_critical_edges(func: &Cfg) -> Output {
        let env = env_with_scratch([None; NUM_CLASSES]);
        let options = RegallocOptions {
            algorithm: Algorithm::Regicide,
            validate_ssa: true,
            allow_critical_edges: true,
            ..RegallocOptions::default()
        };
        let output = crate::run(func, &env, &options).expect("regicide should allocate this");
        let mut checker = Checker::new(func, &env);
        checker.prepare(&output);
        checker.run().expect("checker should accept this");
        output
    }

    fn fixed_def(vreg: VReg, preg: usize) -> Vec<Operand> {
        vec![Operand::new(
            vreg,
            OperandConstraint::FixedReg(PReg::new(preg, vreg.class())),
            OperandKind::Def,
            OperandPos::Late,
        )]
    }

    fn any_use(vreg: VReg) -> Operand {
        Operand::new(
            vreg,
            OperandConstraint::Any,
            OperandKind::Use,
            OperandPos::Early,
        )
    }

    /// block0 defines `a` and `b`, then branches to block1 twice, passing `a`
    /// along one edge and `b` along the other. Both edges are critical, and
    /// since `a` and `b` cannot share a register, at least one of them needs a
    /// move.
    fn twice_to_one_block() -> Cfg {
        let a = VReg::new(0, RegClass::Int);
        let b = VReg::new(1, RegClass::Int);
        let p = VReg::new(2, RegClass::Int);
        Cfg::new(vec![
            (
                vec![],
                vec![fixed_def(a, 0), fixed_def(b, 1), vec![any_use(a)]],
                vec![(1, vec![a]), (1, vec![b])],
            ),
            (vec![p], vec![vec![any_use(p)], vec![]], vec![]),
        ])
    }

    #[test]
    fn critical_edges_are_refused_unless_allowed() {
        let func = twice_to_one_block();
        let env = env_with_scratch([None; NUM_CLASSES]);
        let options = RegallocOptions {
            algorithm: Algorithm::Regicide,
            ..RegallocOptions::default()
        };
        assert!(matches!(
            crate::run(&func, &env, &options),
            Err(RegAllocError::CritEdge(..) | RegAllocError::DisallowedBranchArg(..))
        ));
    }

    #[test]
    fn two_critical_edges_to_one_block() {
        let output = alloc_with_critical_edges(&twice_to_one_block());
        assert!(
            !output.critical_edge_edits.is_empty(),
            "one of the edges needs a move"
        );
        assert!(output
            .critical_edge_edits
            .iter()
            .all(|e| e.block == Block::new(0)));
        assert_eq!(
            output.critical_edge_edits(Block::new(0)).len(),
            output.critical_edge_edits.len()
        );
        assert!(output.critical_edge_edits(Block::new(1)).is_empty());

        // The checker must see that the edge goes without its move.
        let func = twice_to_one_block();
        let env = env_with_scratch([None; NUM_CLASSES]);
        let mut unsplit = output.clone();
        unsplit.critical_edge_edits.clear();
        let mut checker = Checker::new(&func, &env);
        checker.prepare(&unsplit);
        assert!(checker.run().is_err());
    }

    /// A diamond whose join takes `x` and `y` from one side and swapped from
    /// the other, where both sides branch away again, so that every edge into
    /// the join is critical and one of them has to swap the two.
    #[test]
    fn swapping_on_a_critical_edge() {
        let x = VReg::new(0, RegClass::Int);
        let y = VReg::new(1, RegClass::Int);
        let p = VReg::new(2, RegClass::Int);
        let q = VReg::new(3, RegClass::Int);
        let func = Cfg::new(vec![
            (
                vec![],
                vec![fixed_def(x, 0), fixed_def(y, 1), vec![any_use(x)]],
                vec![(1, vec![]), (2, vec![x, y])],
            ),
            (
                vec![],
                vec![vec![any_use(y)]],
                vec![(2, vec![y, x]), (3, vec![])],
            ),
            (
                vec![p, q],
                vec![vec![any_use(p), any_use(q)], vec![]],
                vec![],
            ),
            (vec![], vec![vec![]], vec![]),
        ]);
        let output = alloc_with_critical_edges(&func);
        assert!(!output.critical_edge_edits.is_empty());
        let mut keys: Vec<_> = output
            .critical_edge_edits
            .iter()
            .map(|e| (e.block, e.succ_idx))
            .collect();
        let sorted = {
            let mut sorted = keys.clone();
            sorted.sort();
            sorted
        };
        assert_eq!(keys, sorted, "critical-edge edits are sorted by edge");
        keys.dedup();
        for (block, succ_idx) in keys {
            assert_eq!(func.block_succs(block)[succ_idx as usize], Block::new(2));
        }
    }
}
