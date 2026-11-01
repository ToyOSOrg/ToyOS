//! Timing protection for the calling thread, scoped. [`protect`] turns on the
//! CPU's data-independent timing mode where the thread can set one, and
//! answers a [`Protection`] that puts back the state it found when it drops:
//! protections nest, and an early return or an unwinding panic restores the
//! state. Any program may use it; it asks for no right.
//!
//! The mode makes the instructions its vendor lists take time independent of
//! their data operands, and nothing more: code must already be constant-time
//! in its branches, addresses and choice of instructions. On AArch64 it is
//! `PSTATE.DIT` (FEAT_DIT), which the thread writes itself, so a protection
//! costs a few instructions. x86-64 has none a thread sets
//! (`toyos_abi::syscall::timing_control`), and there
//! [`Protection::is_active`] is false: nothing here reports a machine without
//! the mode as protected.
//!
//! Protections drop in the reverse of the order they were made, as scopes
//! end: dropping an outer one first ends the protection while an inner one is
//! alive. A protection is its thread's, so it is not `Send`.

use core::marker::PhantomData;

use crate::arch::timing;

/// The calling thread's timing protection until this drops. See the module.
#[must_use = "the protection ends when this drops"]
pub struct Protection {
    active: bool,
    /// The mode to turn off again, where it was off before this.
    restore: Option<timing::Mode>,
    _thread: PhantomData<*const ()>,
}

impl Protection {
    /// Whether the mode is on for this protection's life: false where the
    /// machine has none.
    pub fn is_active(&self) -> bool {
        self.active
    }
}

/// Turn the calling thread's timing protection on where the machine has it,
/// until the answer drops.
#[inline]
pub fn protect() -> Protection {
    let mode = timing::mode();
    let restore = mode.filter(|mode| !mode.is_on());
    if let Some(mode) = restore {
        mode.set(true);
    }
    Protection { active: mode.is_some(), restore, _thread: PhantomData }
}

impl Drop for Protection {
    #[inline]
    fn drop(&mut self) {
        if let Some(mode) = self.restore {
            mode.set(false);
        }
    }
}
