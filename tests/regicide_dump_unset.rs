//! That `regicide-dump` captures nothing unless `REGICIDE_DUMP` is set.
//!
//! Its own test binary because the variable is read once per process: a test
//! sharing a process with one that sets it would be captured under that
//! setting, whatever this one does.

#![cfg(feature = "regicide-dump")]

use regalloc2::{
    Algorithm, Block, Function, Inst, InstRange, MachineEnv, Operand, OperandConstraint,
    OperandKind, OperandPos, PReg, PRegSet, RegClass, RegallocOptions, VReg,
};
use std::{env, fs};

/// One block that defines a value, uses it, and returns.
struct Probe {
    def: [Operand; 1],
    use_: [Operand; 1],
}

impl Probe {
    fn new() -> Self {
        let v0 = VReg::new(0, RegClass::Int);
        Probe {
            def: [Operand::new(
                v0,
                OperandConstraint::Reg,
                OperandKind::Def,
                OperandPos::Late,
            )],
            use_: [Operand::new(
                v0,
                OperandConstraint::Reg,
                OperandKind::Use,
                OperandPos::Early,
            )],
        }
    }
}

impl Function for Probe {
    fn name(&self) -> Option<&str> {
        Some("probe")
    }

    fn num_insts(&self) -> usize {
        3
    }

    fn num_blocks(&self) -> usize {
        1
    }

    fn entry_block(&self) -> Block {
        Block::new(0)
    }

    fn block_insns(&self, _: Block) -> InstRange {
        InstRange::new(Inst::new(0), Inst::new(3))
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
        inst.index() == 2
    }

    fn is_branch(&self, _: Inst) -> bool {
        false
    }

    fn branch_blockparams(&self, _: Block, _: Inst, _: usize) -> &[VReg] {
        &[]
    }

    fn inst_operands(&self, inst: Inst) -> &[Operand] {
        match inst.index() {
            0 => &self.def,
            1 => &self.use_,
            _ => &[],
        }
    }

    fn inst_clobbers(&self, _: Inst) -> PRegSet {
        PRegSet::empty()
    }

    fn num_vregs(&self) -> usize {
        1
    }

    fn spillslot_size(&self, _: RegClass) -> usize {
        1
    }
}

/// With `REGICIDE_DUMP` unset, an allocation writes nothing.
#[test]
fn nothing_is_captured_when_unset() {
    env::remove_var("REGICIDE_DUMP");

    // Where the hook would write if it read the variable later than it does.
    let dir = env::temp_dir().join(format!("regalloc2-dump-unset-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);

    let ints: PRegSet = (0..4).map(|i| PReg::new(i, RegClass::Int)).collect();
    let env = MachineEnv {
        preferred_regs_by_class: [ints, PRegSet::empty(), PRegSet::empty()],
        non_preferred_regs_by_class: [PRegSet::empty(); 3],
        scratch_by_class: [None; 3],
        fixed_stack_slots: vec![],
    };
    let options = RegallocOptions {
        algorithm: Algorithm::Regicide,
        validate_ssa: true,
        ..RegallocOptions::default()
    };
    regalloc2::run(&Probe::new(), &env, &options).expect("this function is allocatable");

    assert!(
        !dir.exists(),
        "no directory should have been created for a variable that was never set"
    );
    env::set_var("REGICIDE_DUMP", &dir);
    regalloc2::run(&Probe::new(), &env, &options).expect("this function is allocatable");
    assert!(
        !dir.exists(),
        "the variable is read once per process, so setting it after the first \
         allocation should still capture nothing"
    );
}
