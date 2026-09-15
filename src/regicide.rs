//! Running the `regicide` register allocator through regalloc2's interface.
//!
//! regicide has its own, richer client interface: register *groups* and banks,
//! rematerialization, explicit critical-edge edits. This module is the shim
//! that adapts a [`Function`][crate::Function] and a
//! [`MachineEnv`][crate::MachineEnv] into a `regicide::Input`, runs regicide
//! over it, and translates the result back into a regalloc2
//! [`Output`][crate::Output].

use crate::{Ctx, Function, MachineEnv, RegAllocError, RegallocOptions};

/// The scratch space a run of regicide needs, kept across runs.
///
/// This hangs off [`Ctx`][crate::Ctx] and is reused rather than rebuilt,
/// the same discipline `Ctx` already applies to ion's own allocations.
#[derive(Default)]
pub(crate) struct RegicideCtx {
    /// regicide's own context, which owns every buffer its passes allocate.
    pub(crate) context: regicide::Context,
}

/// Allocate registers for `func` with regicide, writing the result into
/// `ctx.output`.
///
/// Mirrors [`ion::run`][crate::ion::run]: the caller owns the output buffer, so
/// a client that allocates for many functions pays for it once.
pub(crate) fn run<F: Function>(
    _func: &F,
    _env: &MachineEnv,
    _options: &RegallocOptions,
    _ctx: &mut Ctx,
) -> Result<(), RegAllocError> {
    todo!("the shim itself")
}
