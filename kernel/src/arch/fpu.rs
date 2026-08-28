//! What a transition out of Ring 3 must save and restore in full; a task
//! that has never run in Ring 3 starts from the declared state below.

use crate::log;

/// The whole user machine state this kernel permits, in `FXSAVE64`/
/// `FXRSTOR64` layout. Complete only while `CR4.OSXSAVE` stays clear.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct UserFpState([u8; 512]);

/// The x87 control word `FNINIT` leaves.
const FCW_INITIAL: u16 = 0x037F;

/// `MXCSR`'s SDM-defined power-on default.
const MXCSR_INITIAL: u32 = 0x1F80;

// Field offsets in the FXSAVE64 image (SDM Vol. 1 Table 10-2).
const OFF_FCW: usize = 0;
const OFF_MXCSR: usize = 24;
/// The CPU's own mask; not part of the state the self-check compares.
const OFF_MXCSR_MASK: usize = 28;
const OFF_ST0: usize = 32;
/// Past the last XMM register; `FXSAVE` need not write bytes above this.
const END_XMM: usize = 416;

impl UserFpState {
    /// The declared state a task that has never been in Ring 3 starts from.
    pub const INITIAL: Self = {
        let mut b = [0u8; 512];
        let fcw = FCW_INITIAL.to_le_bytes();
        b[OFF_FCW] = fcw[0];
        b[OFF_FCW + 1] = fcw[1];
        let mxcsr = MXCSR_INITIAL.to_le_bytes();
        b[OFF_MXCSR] = mxcsr[0];
        b[OFF_MXCSR + 1] = mxcsr[1];
        b[OFF_MXCSR + 2] = mxcsr[2];
        b[OFF_MXCSR + 3] = mxcsr[3];
        Self(b)
    };

    /// This CPU's state, now — the Rust mirror of `arch::entry`'s naked
    /// `save_user_state!`, which sizes its reservation from `size_of::<Self>()`.
    pub fn saved_from_cpu() -> Self {
        let mut state = Self([0u8; 512]);
        // SAFETY: `fxsave64` needs 16-byte alignment; `state` is
        // `repr(C, align(16))` over `[u8; 512]`.
        unsafe {
            core::arch::asm!(
                "fxsave64 [{}]",
                in(reg) &raw mut state.0,
                options(nostack),
            );
        }
        state
    }

    /// The bytes two `UserFpState`s are compared on.
    fn defined(&self) -> (&[u8], &[u8]) {
        (&self.0[..OFF_MXCSR_MASK], &self.0[OFF_ST0..END_XMM])
    }

    fn matches(&self, other: &Self) -> bool {
        self.defined() == other.defined()
    }

}

/// [`UserFpState::INITIAL`] in memory, for stackless trampolines to load in
/// one instruction; not `FNINIT`, which leaves x87 empty without clearing it.
pub static INITIAL_IMAGE: UserFpState = UserFpState::INITIAL;

/// Puts this CPU's FPU into the declared state and asserts it matches.
/// Call once per CPU after `CR4.OSFXSR` is set; `LDMXCSR` is `#UD` otherwise.
pub fn init() {
    load_initial();
    self_check();
}

/// `FNINIT` then `LDMXCSR`; neither waits, so this is safe over a pending
/// unmasked exception.
fn load_initial() {
    // SAFETY: `&MXCSR_INITIAL` is valid for a 4-byte read; `CR4.OSFXSR` is the
    // caller's job (see `init`).
    unsafe {
        core::arch::asm!(
            "fninit",
            "ldmxcsr [{}]",
            in(reg) &MXCSR_INITIAL,
            options(nostack, readonly),
        );
    }
}

/// Asserts the CPU's architectural default matches [`UserFpState::INITIAL`];
/// a mismatch would be silently restored onto every new thread.
fn self_check() {
    assert_eq!(
        (&raw const INITIAL_IMAGE) as usize % core::mem::align_of::<UserFpState>(),
        0,
        "fpu: the initial image is misaligned, so every trampoline would #GP",
    );
    let live = UserFpState::saved_from_cpu();
    assert!(
        live.matches(&UserFpState::INITIAL),
        "fpu: cpu{} disagrees about the architectural default state — \
         fcw={:#06x} mxcsr={:#010x}, expected fcw={:#06x} mxcsr={:#010x}",
        super::percpu::cpu_id(),
        u16::from_le_bytes([live.0[OFF_FCW], live.0[OFF_FCW + 1]]),
        u32::from_le_bytes([
            live.0[OFF_MXCSR],
            live.0[OFF_MXCSR + 1],
            live.0[OFF_MXCSR + 2],
            live.0[OFF_MXCSR + 3],
        ]),
        FCW_INITIAL,
        MXCSR_INITIAL,
    );
}

/// XCR0: which state components `XSAVE` would move.
///
/// # Safety
/// `CR4.OSXSAVE` must be set; `xgetbv` is `#UD` otherwise.
unsafe fn xgetbv0() -> u64 {
    let lo: u32;
    let hi: u32;
    core::arch::asm!(
        "xgetbv",
        in("ecx") 0u32,
        out("eax") lo,
        out("edx") hi,
        options(nomem, nostack, preserves_flags),
    );
    ((hi as u64) << 32) | lo as u64
}

/// Logs one line per CPU naming what state exists; per CPU because two CPUs
/// disagreeing about `XCR0` would fault on migration, invisibly if summarized.
pub fn log_state() {
    let (max_leaf, _, _, _) = super::cpu::cpuid(0, 0);
    let (_, _, ecx1, _) = super::cpu::cpuid(1, 0);
    let xsave = ecx1 & (1 << 26) != 0;
    let osxsave = ecx1 & (1 << 27) != 0;
    // SAFETY: gated on `osxsave`, CPUID leaf 1's report that `CR4.OSXSAVE` is set.
    let xcr0 = if osxsave { unsafe { xgetbv0() } } else { 0 };
    let (d0a, d0b, d0c, _) = if max_leaf >= 0xD { super::cpu::cpuid(0xD, 0) } else { (0, 0, 0, 0) };
    let (d1a, _, _, _) = if max_leaf >= 0xD { super::cpu::cpuid(0xD, 1) } else { (0, 0, 0, 0) };
    log!(
        "fpu: cpu{} xsave={} osxsave={} xcr0={:#x} \
         cpuid.d.0=({:#x},{},{}) cpuid.d.1.eax={:#x}",
        super::percpu::cpu_id(),
        xsave as u8,
        osxsave as u8,
        xcr0,
        d0a,
        d0b,
        d0c,
        d1a,
    );
}
