//! The `regicide-dump` capture of an allocation that never returns.
//!
//! A separate test binary from the other two, because the dump directory is
//! read from the environment once per process.
//!
//! The allocator is made not to return by aborting from inside a `Function`
//! method it calls, which stands in for a panic, a hang, or a crash: in all of
//! them the dump has to already be on disk, because the dump is the last thing
//! written before the allocator is handed the input. The child process this
//! needs is the test binary itself, re-run with `ABORTING_CHILD` set.

#![cfg(feature = "regicide-dump")]

use regalloc2::{
    Algorithm, Block, Function, Inst, InstRange, MachineEnv, Operand, OperandConstraint,
    OperandKind, OperandPos, PReg, PRegSet, RegClass, RegallocOptions, VReg,
};
use regicide::BlockFrequency;
use std::{
    env, fs,
    process::{self, Command},
    sync::atomic::{AtomicUsize, Ordering},
};

/// Names the directory the child writes its dump to.
const DIR_VAR: &str = "ABORTING_CHILD";

/// Two blocks, so that the allocation reads a block frequency more than once.
struct Aborts {
    def: [Operand; 1],
    use_: [Operand; 1],
    first: [Block; 1],
    second: [Block; 1],
}

impl Aborts {
    fn new() -> Self {
        let v0 = VReg::new(0, RegClass::Int);
        Aborts {
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
            first: [Block::new(0)],
            second: [Block::new(1)],
        }
    }
}

impl Function for Aborts {
    fn name(&self) -> Option<&str> {
        Some("aborts")
    }

    /// Kill the process partway through the allocation.
    ///
    /// Serializing the input reads every block's frequency, so the dump is
    /// complete once more reads than there are blocks have gone by; those
    /// later reads come from the allocation itself.
    ///
    /// Aborting on an earlier read would instead catch the dump half written,
    /// which is what `every_allocation_is_captured` would miss if the capture
    /// ever moved after the run.
    fn block_frequency(&self, _: Block) -> BlockFrequency {
        static READS: AtomicUsize = AtomicUsize::new(0);
        if READS.fetch_add(1, Ordering::Relaxed) >= 2 {
            process::abort();
        }
        BlockFrequency::NORMAL
    }

    fn num_insts(&self) -> usize {
        3
    }

    fn num_blocks(&self) -> usize {
        2
    }

    fn entry_block(&self) -> Block {
        Block::new(0)
    }

    fn block_insns(&self, block: Block) -> InstRange {
        match block.index() {
            0 => InstRange::new(Inst::new(0), Inst::new(1)),
            _ => InstRange::new(Inst::new(1), Inst::new(3)),
        }
    }

    fn block_succs(&self, block: Block) -> &[Block] {
        match block.index() {
            0 => &self.second,
            _ => &[],
        }
    }

    fn block_preds(&self, block: Block) -> &[Block] {
        match block.index() {
            0 => &[],
            _ => &self.first,
        }
    }

    fn block_params(&self, _: Block) -> &[VReg] {
        &[]
    }

    fn is_ret(&self, inst: Inst) -> bool {
        inst.index() == 2
    }

    fn is_branch(&self, inst: Inst) -> bool {
        inst.index() == 0
    }

    fn branch_blockparams(&self, _: Block, _: Inst, _: usize) -> &[VReg] {
        &[]
    }

    fn inst_operands(&self, inst: Inst) -> &[Operand] {
        match inst.index() {
            1 => &self.def,
            2 => &self.use_,
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

/// Allocate, and never come back.
fn child(dir: String) -> ! {
    env::set_var("REGICIDE_DUMP", dir);

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
    let _ = regalloc2::run(&Aborts::new(), &env, &options);

    unreachable!("the allocation should have aborted this process");
}

/// An allocation that never returns still leaves its input on disk, under a
/// name that says so.
#[test]
fn an_allocation_that_never_returns_is_still_captured() {
    if let Ok(dir) = env::var(DIR_VAR) {
        child(dir);
    }

    let dir = env::temp_dir().join(format!("regalloc2-dump-abort-{}", process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let status = Command::new(env::current_exe().unwrap())
        .arg("--exact")
        .arg("an_allocation_that_never_returns_is_still_captured")
        .env(DIR_VAR, &dir)
        .output()
        .expect("the test binary should re-run");
    assert!(
        !status.status.success(),
        "the child should have aborted, but it exited successfully"
    );

    let dumps: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(dumps.len(), 1, "expected one dump, got {:?}", dumps);

    let name = &dumps[0];
    assert!(
        name.ends_with("-aborts.json"),
        "a dump is named the same however its allocation turns out, got {}",
        name
    );

    // Complete despite the abort, and so replayable: this is the whole point
    // of capturing before the run rather than after it.
    let json = fs::read_to_string(dir.join(name)).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).expect("the dump should be JSON");
    assert!(
        value.get("machine").is_some() && value.get("func").is_some(),
        "the dump should hold both the machine and the function"
    );

    fs::remove_dir_all(&dir).unwrap();
}
