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

/// Every field `FNINIT` and `LDMXCSR` leave architecturally defined, as name,
/// offset and width — and the whole of what [`self_check`] may compare.
///
/// `FNINIT` sets `FCW` to `0x037F` and zeroes `FSW`, `FTW`, `FIP`, `FDP` and
/// `FOP` (SDM Vol. 2A, `FINIT/FNINIT`); `LDMXCSR` sets `MXCSR`. **Nothing else
/// in the image is defined by either instruction**, and each exclusion is a
/// field a real CPU is free to hand back differently from a fresh emulator:
///
/// - the x87 data registers at 32 and the XMM registers at 160, because
///   `FNINIT` marks the stack empty *without clearing it* — the same fact
///   [`INITIAL_IMAGE`] exists for — so they hold whatever firmware last left
///   in them;
/// - `MXCSR_MASK` at 28, which is the model's own and reads 0, `0xFFFF` or
///   `0x1FFFF` depending on the part;
/// - the reserved byte at 5 and everything from 464, which no specification
///   defines at all.
///
/// `FTW` here is the abridged one-byte tag word `FXSAVE` writes, not the x87
/// tag register: all-empty is `0x00` in it and `0xFFFF` in the register.
const DEFINED: &[(&str, usize, usize)] = &[
    ("FCW", 0, 2),
    ("FSW", 2, 2),
    ("FTW", 4, 1),
    ("FOP", 6, 2),
    ("FIP", 8, 8),
    ("FDP", 16, 8),
    ("MXCSR", OFF_MXCSR, 4),
];

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

    /// One [`DEFINED`] field, as the little-endian integer it is.
    fn field(&self, at: usize, width: usize) -> u64 {
        let mut value = 0u64;
        let mut i = 0;
        while i < width {
            // Every (offset, width) in `DEFINED` is inside a 512-byte image.
            value |= (self.0[at + i] as u64) << (8 * i);
            i += 1;
        }
        value
    }

    /// The first [`DEFINED`] field these two disagree about: name, offset, this
    /// one's value, the other's. Named rather than reported as a byte range,
    /// because a range says which bytes differ and never which field they were.
    fn first_difference(&self, other: &Self) -> Option<(&'static str, usize, u64, u64)> {
        DEFINED.iter().find_map(|&(name, at, width)| {
            let (got, want) = (self.field(at, width), other.field(at, width));
            (got != want).then_some((name, at, got, want))
        })
    }
}

/// [`UserFpState::INITIAL`] in memory, for stackless trampolines to load in
/// one instruction; not `FNINIT`, which leaves x87 empty without clearing it.
pub static INITIAL_IMAGE: UserFpState = UserFpState::INITIAL;

/// Puts this CPU's FPU into the declared state and asserts it matches.
/// Call once per CPU after `CR4.OSFXSR` is set; `LDMXCSR` is `#UD` otherwise.
pub fn init(cpu_id: u32) {
    load_initial();
    self_check(cpu_id);
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
///
/// Takes its own id for the same reason [`log_state`] does: the BSP runs this
/// before the `wrmsr` that makes `gs:` valid, so reading the id from there
/// would fault inside the message of the assertion reporting the mismatch.
fn self_check(cpu_id: u32) {
    assert_eq!(
        (&raw const INITIAL_IMAGE) as usize % core::mem::align_of::<UserFpState>(),
        0,
        "fpu: the initial image is misaligned, so every trampoline would #GP",
    );
    let live = UserFpState::saved_from_cpu();
    if let Some((name, at, got, want)) = live.first_difference(&UserFpState::INITIAL) {
        panic!(
            "fpu: cpu{cpu_id} disagrees about the architectural default state — \
             {name} at offset {at} reads {got:#x}, the declaration is {want:#x}"
        );
    }
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
///
/// Takes its own id rather than reading `gs:`: the BSP calls this between
/// [`init`] and the `wrmsr` that makes `gs:` valid, so that a boot which stops
/// in that gap has still said what this CPU's extended state is.
pub fn log_state(cpu_id: u32) {
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
        cpu_id,
        xsave as u8,
        osxsave as u8,
        xcr0,
        d0a,
        d0b,
        d0c,
        d1a,
    );
}
