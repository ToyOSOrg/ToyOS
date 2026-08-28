//! The one bracket every transition out of Ring 3 uses.
//!
//! A transition out of Ring 3 that can reach another task must save and
//! restore the whole user machine state, as the last act before returning to
//! Ring 3, after any point the task could have switched. [`save_user_state`]
//! leaves `rsp` aligned to [`UserFpState`]; [`restore_user_state`] restores it
//! exactly, and `r11` is scratch between them.

use core::mem::{align_of, size_of};

use super::fpu::UserFpState;

/// A gate whose handler can reach another task before returning to Ring 3,
/// and therefore brackets the user machine state.
#[derive(Clone, Copy)]
pub struct Ring3Entry(unsafe extern "sysv64" fn());

/// A gate whose handler cannot reach another task, so it saves nothing.
#[derive(Clone, Copy)]
pub struct Ring0Entry(unsafe extern "sysv64" fn());

impl Ring3Entry {
    pub const fn new(handler: unsafe extern "sysv64" fn()) -> Self {
        Self(handler)
    }

    pub fn addr(self) -> u64 {
        self.0 as *const () as u64
    }
}

impl Ring0Entry {
    pub const fn declare(handler: unsafe extern "sysv64" fn()) -> Self {
        Self(handler)
    }

    pub fn addr(self) -> u64 {
        self.0 as *const () as u64
    }
}

// `fp_bytes + fp_align`, aligned down, fits any incoming `rsp` alignment,
// with room in the slack above it to stash the caller's `rsp`.
const _: () = assert!(size_of::<UserFpState>().is_multiple_of(align_of::<UserFpState>()));
const _: () = assert!(align_of::<UserFpState>() >= 8);

/// `naked_asm!` for an entry that can reach another task: supplies the save
/// area's size and alignment from [`UserFpState`], and prepends `cld`. The
/// body must end with a trailing comma.
#[cfg(not(feature = "entry-df-unclean"))]
macro_rules! ring3_naked_asm {
    ($($body:tt)*) => {
        core::arch::naked_asm!(
            // An interrupt/trap gate does not clear `DF`, and this kernel's
            // forward-only `memmove`/`memcpy` write backward if a tick
            // interrupts a copy with `DF` set.
            "cld",
            $($body)*
            fp_bytes = const core::mem::size_of::<$crate::arch::fpu::UserFpState>(),
            fp_align = const core::mem::align_of::<$crate::arch::fpu::UserFpState>(),
        )
    };
}

/// Negative control (`entry-df-unclean`): the `cld` above removed;
/// `arch::syscall::init` matches it by taking `DF` back out of `IA32_FMASK`.
#[cfg(feature = "entry-df-unclean")]
macro_rules! ring3_naked_asm {
    ($($body:tt)*) => {
        core::arch::naked_asm!(
            $($body)*
            fp_bytes = const core::mem::size_of::<$crate::arch::fpu::UserFpState>(),
            fp_align = const core::mem::align_of::<$crate::arch::fpu::UserFpState>(),
        )
    };
}

/// `naked_asm!` for a trampoline into Ring 3 for the first time: supplies
/// the declared state's address from `fpu::INITIAL_IMAGE`.
#[cfg(not(feature = "fpu-save-nothing"))]
macro_rules! ring3_trampoline_asm {
    ($($body:tt)*) => {
        core::arch::naked_asm!(
            $($body)*
            fp_initial = sym $crate::arch::fpu::INITIAL_IMAGE,
        )
    };
}

/// The state a task that has never been in Ring 3 starts from.
#[cfg(not(feature = "fpu-save-nothing"))]
macro_rules! initial_user_state {
    () => {
        "fxrstor64 [rip + {fp_initial}]\n"
    };
}

/// Park the user machine state on this kernel stack, leaving `rsp` aligned
/// for the System V `call` that follows.
#[cfg(not(feature = "fpu-save-nothing"))]
macro_rules! save_user_state {
    () => {
        concat!(
            "mov r11, rsp\n",
            "sub rsp, {fp_bytes}\n",
            "sub rsp, {fp_align}\n",
            "and rsp, -{fp_align}\n",
            "mov [rsp + {fp_bytes}], r11\n",
            // fxsave64, not fsave/fxsave: non-waiting (won't trap a pending x87
            // exception) and REX.W-wide (fxsave truncates FIP/FDP to 32 bits).
            "fxsave64 [rsp]\n",
        )
    };
}

/// Put it back, and `rsp` with it.
#[cfg(not(feature = "fpu-save-nothing"))]
macro_rules! restore_user_state {
    () => {
        concat!(
            "fxrstor64 [rsp]\n",
            "mov rsp, [rsp + {fp_bytes}]\n",
        )
    };
}

// Negative control (`fpu-save-nothing`): same reservation, alignment and
// `rsp` bookkeeping as above, without moving the state.

#[cfg(feature = "fpu-save-nothing")]
macro_rules! ring3_trampoline_asm {
    ($($body:tt)*) => { core::arch::naked_asm!($($body)*) };
}

#[cfg(feature = "fpu-save-nothing")]
macro_rules! initial_user_state {
    () => {
        ""
    };
}

#[cfg(feature = "fpu-save-nothing")]
macro_rules! save_user_state {
    () => {
        concat!(
            "mov r11, rsp\n",
            "sub rsp, {fp_bytes}\n",
            "sub rsp, {fp_align}\n",
            "and rsp, -{fp_align}\n",
            "mov [rsp + {fp_bytes}], r11\n",
        )
    };
}

#[cfg(feature = "fpu-save-nothing")]
macro_rules! restore_user_state {
    () => {
        "mov rsp, [rsp + {fp_bytes}]\n"
    };
}

pub(crate) use {
    initial_user_state, restore_user_state, ring3_naked_asm, ring3_trampoline_asm, save_user_state,
};
