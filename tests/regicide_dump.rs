//! The `regicide-dump` capture, end to end.
//!
//! An integration test rather than a unit test because the dump directory is
//! read from the environment once per process: this file holds the one test
//! that sets it, so nothing else has read it first. That is also why the
//! successful and the failing allocation are asserted on by a single test:
//! they share the process, and so the directory and the counter.
//!
//! The allocation that never returns is covered separately, in
//! `regicide_dump_abort.rs`, which needs a process of its own to kill, as is
//! the unset variable, in `regicide_dump_unset.rs`, which needs a process
//! that never sets it.

#![cfg(feature = "regicide-dump")]

use regalloc2::{
    Algorithm, Block, Function, Inst, InstRange, MachineEnv, Operand, OperandConstraint,
    OperandKind, OperandPos, PReg, PRegSet, RegClass, RegallocOptions, VReg,
};
use std::{env, fs, path::Path};

/// The registers the machine grants.
const REGS: usize = 4;

/// A straight-line function of `live` values, all of them live at once at its
/// last instruction.
///
/// With `live` above `REGS` there are more values wanting a register than the
/// machine has, and the allocation fails; below it, it succeeds. So the same
/// shape drives both halves of the test.
struct Straightline {
    name: Option<&'static str>,
    defs: Vec<[Operand; 1]>,
    uses: Vec<Operand>,
}

impl Straightline {
    fn new(name: Option<&'static str>, live: usize) -> Self {
        let vreg = |i| VReg::new(i, RegClass::Int);
        Straightline {
            name,
            defs: (0..live)
                .map(|i| {
                    [Operand::new(
                        vreg(i),
                        OperandConstraint::Reg,
                        OperandKind::Def,
                        OperandPos::Late,
                    )]
                })
                .collect(),
            uses: (0..live)
                .map(|i| {
                    Operand::new(
                        vreg(i),
                        OperandConstraint::Reg,
                        OperandKind::Use,
                        OperandPos::Early,
                    )
                })
                .collect(),
        }
    }

    fn live(&self) -> usize {
        self.defs.len()
    }
}

impl Function for Straightline {
    fn name(&self) -> Option<&str> {
        self.name
    }

    fn num_insts(&self) -> usize {
        self.live() + 2
    }

    fn num_blocks(&self) -> usize {
        1
    }

    fn entry_block(&self) -> Block {
        Block::new(0)
    }

    fn block_insns(&self, _: Block) -> InstRange {
        InstRange::new(Inst::new(0), Inst::new(self.num_insts()))
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

    fn is_ret(&self, inst: Inst) -> bool {
        inst.index() == self.num_insts() - 1
    }

    fn is_branch(&self, _: Inst) -> bool {
        false
    }

    fn branch_blockparams(&self, _: Block, _: Inst, _: usize) -> &[VReg] {
        &[]
    }

    fn inst_operands(&self, inst: Inst) -> &[Operand] {
        match inst.index() {
            i if i < self.live() => &self.defs[i],
            i if i == self.live() => &self.uses,
            _ => &[],
        }
    }

    fn inst_clobbers(&self, _: Inst) -> PRegSet {
        PRegSet::empty()
    }

    fn num_vregs(&self) -> usize {
        self.live()
    }

    fn spillslot_size(&self, _: RegClass) -> usize {
        1
    }
}

fn machine_env() -> MachineEnv {
    let ints: PRegSet = (0..REGS).map(|i| PReg::new(i, RegClass::Int)).collect();
    MachineEnv {
        preferred_regs_by_class: [ints, PRegSet::empty(), PRegSet::empty()],
        non_preferred_regs_by_class: [PRegSet::empty(); 3],
        scratch_by_class: [None; 3],
        fixed_stack_slots: vec![],
    }
}

fn allocate(func: &Straightline) -> Result<(), ()> {
    let options = RegallocOptions {
        algorithm: Algorithm::Regicide,
        validate_ssa: true,
        ..RegallocOptions::default()
    };
    regalloc2::run(func, &machine_env(), &options)
        .map(|_| ())
        .map_err(|_| ())
}

/// The names in `dir`, sorted, so the counter's order is the vec's order.
fn dumps(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .expect("the hook should have created the dump directory")
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// Every allocation is captured, whether it succeeds or fails, under a name
/// carrying the function's name and a counter, and what is written is JSON.
#[test]
fn every_allocation_is_captured() {
    let dir = env::temp_dir().join(format!("regalloc2-dump-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    env::set_var("REGICIDE_DUMP", &dir);

    allocate(&Straightline::new(Some("fits"), REGS - 1)).expect("this function is allocatable");
    allocate(&Straightline::new(Some("u3:0"), REGS * 2))
        .expect_err("more values want registers than the machine has");
    // A function with no name still gets a dump, just without that component.
    allocate(&Straightline::new(None, REGS - 1)).expect("this function is allocatable");

    let pid = std::process::id();
    assert_eq!(
        dumps(&dir),
        vec![
            format!("{pid}-0000-fits.json"),
            format!("{pid}-0001-u3-0.json"),
            format!("{pid}-0002.json"),
        ],
        "a failing allocation is captured the same way as a successful one, \
         and the counter distinguishes them"
    );

    for name in dumps(&dir) {
        let json = fs::read_to_string(dir.join(&name)).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&json).expect("the dump should be JSON");
        assert!(
            value.get("machine").is_some() && value.get("func").is_some(),
            "{} should hold both the machine and the function",
            name
        );
    }

    fs::remove_dir_all(&dir).unwrap();
}
