//! Fuzz the `regicide` register allocator, reached through this crate's shim.

use crate::{
    checker, fuzzing::func, ion, Algorithm, MoveEliminationLevel, RegAllocError, RegallocOptions,
};
use arbitrary::{Arbitrary, Result, Unstructured};
use core::cell::RefCell;
use core::sync::atomic::{AtomicU64, Ordering};
use std::thread_local;

/// `regicide`-specific options for generating functions.
///
/// These are `ion`'s options but for `fixed_reg_max`: the shim cannot express
/// a value pinned to a register the allocator does not own, and
/// [`func::machine_env`] makes only encodings `0..32` allocatable. Leaving the
/// default in place would have the generator name a fixed stack slot about
/// half the time, and a single such operand rejects the whole function — which
/// once cost this target 96% of its coverage.
const OPTIONS: func::Options = func::Options {
    reused_inputs: true,
    fixed_regs: true,
    fixed_nonallocatable: true,
    clobbers: true,
    reftypes: true,
    remat: true,
    callsite_ish_constraints: true,
    fixed_reg_max: 31,
    ..func::Options::DEFAULT
};

/// A convenience wrapper to generate a [`func::Func`] with `regicide`-specific
/// options enabled.
#[derive(Clone, Debug)]
pub struct TestCase {
    func: func::Func,
    check_ssa: bool,
    move_elimination_level: MoveEliminationLevel,
}

impl Arbitrary<'_> for TestCase {
    fn arbitrary(u: &mut Unstructured) -> Result<TestCase> {
        let func = func::Func::arbitrary_with_options(u, &OPTIONS)?;
        let check_ssa = bool::arbitrary(u)?;
        let move_elimination_level = *u.choose(&[
            MoveEliminationLevel::None,
            MoveEliminationLevel::Local,
            MoveEliminationLevel::Global,
        ])?;
        Ok(TestCase {
            func,
            check_ssa,
            move_elimination_level,
        })
    }
}

/// How many functions this target has handed to the shim, and how many of
/// those the shim refused as [`RegAllocError::RegicideUnsupported`].
///
/// A refusal is not a failure — regalloc2's interface admits things regicide's
/// does not — but it is also not coverage, and it is silent. A new rejection
/// reason would otherwise take the target's inputs away one by one while it
/// went on reporting success, so [`check_acceptance_rate`] puts a floor under
/// it.
static SEEN: AtomicU64 = AtomicU64::new(0);
static UNSUPPORTED: AtomicU64 = AtomicU64::new(0);

/// The share of generated functions the shim is expected to accept.
///
/// Measured at 100% over a 60s sweep once fixed registers were confined to the
/// allocatable range; the margin is for the rejections that remain reachable
/// in principle, not for a regression.
const MIN_ACCEPTANCE_RATE: f64 = 0.90;

/// How often to re-check the rate. A fuzz run has no end this target can hook,
/// so it checks as it goes; the interval is large enough that the check costs
/// nothing and that early noise has averaged out.
const ACCEPTANCE_CHECK_INTERVAL: u64 = 1024;

/// Test a single function with the `regicide` allocator.
///
/// This also:
/// - optionally verifies the incoming SSA
/// - runs the [`checker`].
pub fn check(t: TestCase) {
    let TestCase {
        func,
        check_ssa,
        move_elimination_level,
    } = &t;
    log::trace!("func:\n{func:?}");

    let env = func::machine_env();
    let options = RegallocOptions {
        verbose_log: false,
        validate_ssa: *check_ssa,
        algorithm: Algorithm::Regicide,
        move_elimination_level: *move_elimination_level,
    };

    thread_local! {
        // `RegicideCtx` lives in `Ctx`, so reusing one across runs also tests
        // that the machine cache and the shim's scratch buffers are reset.
        static CTX: RefCell<ion::Ctx> = RefCell::default();
    }

    let seen = SEEN.fetch_add(1, Ordering::Relaxed) + 1;
    if seen % ACCEPTANCE_CHECK_INTERVAL == 0 {
        check_acceptance_rate();
    }

    CTX.with(|ctx| {
        let mut ctx = ctx.borrow_mut();
        match crate::run_with_ctx(func, &env, &options, &mut ctx) {
            Ok(output) => {
                let mut checker = checker::Checker::new(func, &env);
                checker.prepare(output);
                checker.run().expect("checker failed");
            }
            // regalloc2 can say things regicide cannot hear. Skipping is the
            // contract, but see `MIN_ACCEPTANCE_RATE`.
            Err(RegAllocError::RegicideUnsupported(msg)) => {
                UNSUPPORTED.fetch_add(1, Ordering::Relaxed);
                log::debug!("unsupported: {msg}");
            }
            Err(e) => panic!("regalloc did not succeed: {:?}", e),
        }
    });
}

/// Panic if the shim has been refusing so much of what the generator produces
/// that a green run would not mean much.
///
/// [`check`] calls this every [`ACCEPTANCE_CHECK_INTERVAL`] functions, and it
/// is a no-op below a sample worth believing.
pub fn check_acceptance_rate() {
    let seen = SEEN.load(Ordering::Relaxed);
    let unsupported = UNSUPPORTED.load(Ordering::Relaxed);
    if seen < 100 {
        return;
    }
    let rate = (seen - unsupported) as f64 / seen as f64;
    assert!(
        rate >= MIN_ACCEPTANCE_RATE,
        "the shim accepted {} of {} generated functions ({:.1}%), below the {:.0}% floor; \
         a fuzz run that mostly refuses its inputs is not testing regicide. \
         Run with `RUST_LOG=debug` to see the rejection reasons.",
        seen - unsupported,
        seen,
        100.0 * rate,
        100.0 * MIN_ACCEPTANCE_RATE,
    );
}

#[test]
fn smoke() {
    arbtest::arbtest(|u| {
        let test_case = TestCase::arbitrary(u)?;
        check(test_case);
        Ok(())
    })
    .budget_ms(1_000);

    check_acceptance_rate();
}
