//! Running the `regicide` register allocator through regalloc2's interface.
//!
//! regicide has its own, richer client interface: register *groups* and banks,
//! rematerialization, explicit critical-edge edits. This module is the shim
//! that adapts a [`Function`][crate::Function] and a
//! [`MachineEnv`][crate::MachineEnv] into a `regicide::Input`, runs regicide
//! over it, and translates the result back into a regalloc2
//! [`Output`][crate::Output].

/// The scratch space a run of regicide needs, kept across runs.
///
/// This hangs off [`Ctx`][crate::Ctx] and is reused rather than rebuilt,
/// the same discipline `Ctx` already applies to ion's own allocations.
#[derive(Default)]
pub(crate) struct RegicideCtx {
    /// regicide's own context, which owns every buffer its passes allocate.
    pub(crate) context: regicide::Context,
}
