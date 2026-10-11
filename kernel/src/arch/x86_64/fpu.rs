//! What a transition out of Ring 3 must save and restore in full; a task
//! that has never run in Ring 3 starts from the declared state below.
//!
//! The user machine state is every component `XCR0` enables
//! (`control_regs`, `kernel::xsave`): `arch::entry` saves and restores it by
//! the machine's [`FORM`] into [`AREA_BYTES`] parked on the thread's kernel
//! stack, compacted under `XSAVES` and `XSAVEC`, standard under `XSAVEOPT`.

use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};

use kernel::xsave::{Component, Form, COMPACTED_BASE, IMAGE_BYTES};

use crate::log;

/// The legacy region of an `XSAVE` area, which is the `FXSAVE64` image.
#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Legacy([u8; 512]);

/// An `XSAVE` area of `N` bytes, aligned as every save instruction needs.
#[repr(C, align(64))]
pub struct XsaveImage<const N: usize>([u8; N]);

/// The x87 control word `FNINIT` leaves.
const FCW_INITIAL: u16 = 0x037F;

/// `MXCSR`'s SDM-defined power-on default.
const MXCSR_INITIAL: u32 = 0x1F80;

// Field offsets in the FXSAVE64 image (SDM Vol. 1 Table 10-2) and the XSAVE
// header after it (SDM Vol. 1 §13.4.2).
const OFF_FCW: usize = 0;
const OFF_MXCSR: usize = 24;
const OFF_XSTATE_BV: usize = 512;
const OFF_XCOMP_BV: usize = 520;

/// The components a first-entry image loads from its own bytes: x87 and SSE.
/// Every other component `XCR0` enables is put in its initial configuration,
/// which is zero (SDM Vol. 1 §13.6).
const XSTATE_BV_INITIAL: u64 = 0b11;

/// Every field [`self_check`] compares, as name, offset and width: the whole
/// legacy region but `MXCSR_MASK` at 28, which is the model's own and reads 0,
/// `0xFFFF` or `0x1FFFF` depending on the part, the six reserved bytes of each
/// x87 register's slot and the reserved byte at 5, and everything from 416,
/// which no specification defines.
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
    ("ST0", 32, 10),
    ("ST1", 48, 10),
    ("ST2", 64, 10),
    ("ST3", 80, 10),
    ("ST4", 96, 10),
    ("ST5", 112, 10),
    ("ST6", 128, 10),
    ("ST7", 144, 10),
    ("XMM0-XMM15", 160, 256),
];

impl Legacy {
    /// The declared x87 and SSE state: `FNINIT`'s control word, `MXCSR`'s
    /// default, every register zero.
    const INITIAL: Self = {
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

    /// This CPU's x87 and SSE state, now. `FXSAVE64` writes the whole legacy
    /// region whatever a component's use, where `XSAVE` may skip one in its
    /// initial configuration.
    fn saved_from_cpu() -> Self {
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

    /// The first [`DEFINED`] field these two disagree about, by name and
    /// offset, with both values.
    fn first_difference<'a>(&'a self, other: &'a Self) -> Option<(&'static str, usize, &'a [u8], &'a [u8])> {
        DEFINED.iter().find_map(|&(name, at, width)| {
            let (got, want) = (&self.0[at..at + width], &other.0[at..at + width]);
            (got != want).then_some((name, at, got, want))
        })
    }
}

/// The declared initial state as an `N`-byte area with `XCOMP_BV` as given.
const fn initial_image<const N: usize>(xcomp_bv: u64) -> XsaveImage<N> {
    let mut b = [0u8; N];
    let mut i = 0;
    while i < Legacy::INITIAL.0.len() {
        b[i] = Legacy::INITIAL.0[i];
        i += 1;
    }
    let xstate_bv = XSTATE_BV_INITIAL.to_le_bytes();
    let xcomp_bv = xcomp_bv.to_le_bytes();
    let mut k = 0;
    while k < 8 {
        b[OFF_XSTATE_BV + k] = xstate_bv[k];
        b[OFF_XCOMP_BV + k] = xcomp_bv[k];
        k += 1;
    }
    XsaveImage(b)
}

/// The state a task that has never been in Ring 3 starts from under
/// `XSAVEOPT`, in memory for stackless trampolines to load in one `XRSTOR`:
/// standard form, as long as any area a standard save writes.
pub static INITIAL_STANDARD: XsaveImage<IMAGE_BYTES> = initial_image(0);

/// The same state under `XSAVES` and `XSAVEC`: compacted form, holding x87
/// and SSE alone, so the restore puts every component past them in its
/// initial configuration and reads nothing past the header (SDM Vol. 1
/// §13.8.2).
pub static INITIAL_COMPACTED: XsaveImage<{ COMPACTED_BASE as usize }> =
    initial_image(1 << 63 | XSTATE_BV_INITIAL);

/// The machine's save form, as [`Form`]'s discriminant; zero until the BSP's
/// [`declare_area`]. Read by every entry stub and trampoline
/// (`arch::entry`), which branch on it.
pub static FORM: AtomicU8 = AtomicU8::new(0);

/// The bytes one save writes on every CPU of this machine, by the declared
/// form for the declared `XCR0`. Zero until the BSP's [`declare_area`]; read
/// by every entry stub (`arch::entry::save_user_state`), which reserves this
/// much and the line below it.
pub static AREA_BYTES: AtomicU64 = AtomicU64::new(0);

/// The machine's save form. Panics before the BSP's [`declare_area`].
pub fn form() -> Form {
    match FORM.load(Ordering::Acquire) {
        f if f == Form::Xsaves as u8 => Form::Xsaves,
        f if f == Form::Xsavec as u8 => Form::Xsavec,
        f if f == Form::Xsaveopt as u8 => Form::Xsaveopt,
        f => panic!("fpu: the save form was read as {f} before the BSP declared it"),
    }
}

/// Declares the save form and the area this CPU's `XCR0` implies under it,
/// and checks both against the machine's. Runs once `XCR0` and `IA32_XSS`
/// hold the declaration, and before any entry stub that saves can run.
pub fn declare_area(cpu_id: u32, xcr0: u64, form: Form) {
    let area = if form.compacted() {
        let component = |i| {
            let (eax, _, ecx, _) = super::cpu::leaf_d(i);
            Component::from_cpuid(eax, ecx)
        };
        kernel::xsave::compacted_bytes(xcr0, component, super::cpu::leaf_d(1).1)
    } else {
        kernel::xsave::standard_bytes(super::cpu::leaf_d(0).1)
    };
    let bytes = match area {
        Ok(bytes) => bytes as u64,
        Err(refusal) => panic!("fpu: cpu{cpu_id} cannot save by {}: {} ({refusal:?})", form.name(), refusal.reason()),
    };
    if let Err(machine) = FORM.compare_exchange(0, form as u8, Ordering::Release, Ordering::Acquire) {
        assert!(
            machine == form as u8,
            "fpu: cpu{cpu_id} saves by {} and the machine declared {}",
            form.name(),
            self::form().name(),
        );
    }
    if let Err(machine) = AREA_BYTES.compare_exchange(0, bytes, Ordering::Release, Ordering::Acquire) {
        assert!(
            machine == bytes,
            "fpu: cpu{cpu_id} saves {bytes} bytes and the machine declared {machine}",
        );
    }
}

/// Puts this CPU into the declared state and asserts it matches. Call once
/// per CPU after [`declare_area`].
pub fn init(cpu_id: u32) {
    load_initial();
    self_check(cpu_id);
}

/// The first-entry image by the instruction the first-entry trampolines use.
fn load_initial() {
    // SAFETY: both images are 64-byte aligned and name only x87 and SSE, both
    // in every declared `XCR0`, with an `MXCSR` that sets no reserved bit; the
    // standard one is as long as any standard area this machine's `XCR0`
    // reaches (`declare_area` refused a longer one), and the compacted one
    // holds every component its `XCOMP_BV` names. `XRSTORS` runs only under
    // the form that found `XSAVES`.
    unsafe {
        match form() {
            Form::Xsaves => core::arch::asm!(
                "xrstors64 [{}]",
                in(reg) &raw const INITIAL_COMPACTED.0,
                in("eax") u32::MAX,
                in("edx") u32::MAX,
                options(nostack, readonly),
            ),
            Form::Xsavec => core::arch::asm!(
                "xrstor64 [{}]",
                in(reg) &raw const INITIAL_COMPACTED.0,
                in("eax") u32::MAX,
                in("edx") u32::MAX,
                options(nostack, readonly),
            ),
            Form::Xsaveopt => core::arch::asm!(
                "xrstor64 [{}]",
                in(reg) &raw const INITIAL_STANDARD.0,
                in("eax") u32::MAX,
                in("edx") u32::MAX,
                options(nostack, readonly),
            ),
        }
    }
}

/// Asserts what loading the first-entry image left on this CPU reads back as the
/// declaration; a mismatch would be silently restored onto every new thread.
///
/// Takes its own id for the same reason [`log_state`] does: the BSP runs this
/// before the `wrmsr` that makes `gs:` valid, so reading the id from there
/// would fault inside the message of the assertion reporting the mismatch.
fn self_check(cpu_id: u32) {
    let live = Legacy::saved_from_cpu();
    if let Some((name, at, got, want)) = live.first_difference(&Legacy::INITIAL) {
        panic!(
            "fpu: cpu{cpu_id} disagrees about the declared initial state — \
             {name} at offset {at} reads {got:02x?}, the declaration is {want:02x?}"
        );
    }
}

/// Logs one line per CPU naming what state exists; per CPU because two CPUs
/// disagreeing about `XCR0` would fault on migration, invisibly if summarized.
///
/// Takes its own id rather than reading `gs:`: the BSP calls this between
/// [`init`] and the `wrmsr` that makes `gs:` valid, so that a boot which stops
/// in that gap has still said what this CPU's extended state is.
pub fn log_state(cpu_id: u32) {
    let (eax1, _, ecx1, _) = super::cpu::cpuid(1, 0);
    let xsave = ecx1 & (1 << 26) != 0;
    let osxsave = ecx1 & (1 << 27) != 0;
    // SAFETY: gated on `osxsave`, CPUID leaf 1's report that `CR4.OSXSAVE` is set.
    let xcr0 = if osxsave { unsafe { super::cpu::xgetbv0() } } else { 0 };
    let (d0a, d0b, d0c, _) = super::cpu::leaf_d(0);
    let (d1a, d1b, _, _) = super::cpu::leaf_d(1);
    let mut components = [(0u32, 0u32, 0u32); 8];
    for (slot, i) in components.iter_mut().zip(2..) {
        let (eax, ebx, ecx, _) = if xcr0 & (1 << i) != 0 { super::cpu::leaf_d(i) } else { (0, 0, 0, 0) };
        *slot = (eax, ebx, ecx);
    }
    log!(
        "fpu: cpu{} cpuid.1.eax={:#x} xsave={} osxsave={} xcr0={:#x} \
         cpuid.d.0=({:#x},{},{}) cpuid.d.1=({:#x},{}) cpuid.d.2..9.(eax,ebx,ecx)={:?} save={} area={}",
        cpu_id,
        eax1,
        xsave as u8,
        osxsave as u8,
        xcr0,
        d0a,
        d0b,
        d0c,
        d1a,
        d1b,
        components,
        form().name(),
        AREA_BYTES.load(Ordering::Relaxed),
    );
}

/// [`DEFINED`] stays inside the legacy region and stays ascending and
/// non-overlapping: `first_difference` slices each field by offset and width.
const _: () = {
    let mut i = 0;
    let mut end = 0;
    while i < DEFINED.len() {
        let (_, at, width) = DEFINED[i];
        assert!(at >= end, "DEFINED is not ascending and non-overlapping");
        assert!(width > 0, "an empty field");
        end = at + width;
        assert!(end <= 512, "a field outside the legacy region");
        i += 1;
    }
};

// The header the trampolines' restore reads lies inside each image.
const _: () = assert!(OFF_XSTATE_BV + 64 <= COMPACTED_BASE as usize);
const _: () = assert!(COMPACTED_BASE as usize <= IMAGE_BYTES);
// `arch::entry`'s stubs compare `FORM` against these.
const _: () = assert!(Form::Xsaves as u8 == 1 && Form::Xsavec as u8 == 2 && Form::Xsaveopt as u8 == 3);
