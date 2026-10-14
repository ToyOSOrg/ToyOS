//! This CPU's microcode, loaded from the patch the loader found on ROOT for
//! the machine's CPU (`KernelArgs::microcode_addr`) before anything reads
//! what an update changes: Intel asks it "on each core as early as possible
//! in the OS boot sequence, before any CPUID feature flags or other
//! enumeration values are cached". The BSP loads in `boot::after_console`,
//! before `random::key` draws on RDRAND and RDSEED and before
//! `control_regs::init` reads CPUID; each AP in `smp::ap_entry`, before its
//! own. `boot_aps` starts one AP at a time, the serialisation per core SDM
//! Vol. 3A §12.11.6.3 asks. What a CPU decides is `kernel::microcode`'s.
//!
//! **Both run before this kernel's IDT**, so a fault inside Intel's trigger
//! reports nothing: the BSP's vectors into firmware's table, an AP's into the
//! kernel's with `CR4.OSFXSR` still clear, a triple fault. Intel documents no
//! fault for a valid update at a present, canonical address. AMD documents a
//! `#GP` for a patch the CPU refuses (57896 §2.1.3.2), so AMD's write runs
//! under a table of its own whose one gate catches that write's `#GP` and
//! nothing else.
//!
//! **A CPU's verdict on an update is the revision it reads back, and a
//! refusal is reported, not a panic.** Intel judges a load by MSR 8BH read
//! after it, equal to the update's revision or a failure (SDM Vol. 3A
//! §12.11.7.2, Example 12-10), and the register reports "the currently
//! functioning revision" (§12.11.7.1); AMD's MSR0000_008B reports "the
//! microcode patch that has been loaded" (55922 §2.1.14.1). So a CPU that
//! does not take an update, or faults on it, runs the revision it reads
//! back: its line says `refused` with the update it was handed, its
//! `running` is that read-back, and it is held to the machine's like any
//! other.
//!
//! One line per CPU says what it found, the patch it was handed and what it
//! runs; a CPU that loads nothing says why, and a CPU of a vendor ToyOS has
//! no loader for says so by the vendor's name and reads nothing more. Every
//! CPU that reads a revision runs the one the first did, or the boot stops
//! naming it: whatever reads the revision assumes one.
//!
//! **AMD documents its patch loader for Families 19h and 1Ah alone** (55901
//! §2.1.15.3, 57896 §2.1.13.3). On the AMD bench, Family 17h Model 60h on
//! firmware without AMD-SB-7033's fix, ToyOSOrg/evidence `32c335ece`
//! (`metal/amd-bench/`) measured what no Family 17h document states: writing
//! a patch's linear address to MSRC001_0020 loads it, page-aligned or 0xC
//! past a 16-byte boundary; a refused patch raises `#GP(0)` and leaves the
//! level; and a write at the level a core runs, its second thread's after its
//! first loaded among them, changes nothing and does not fault. Firmware with
//! that fix and a core's patch through CC6 are unmeasured
//! (`issues/amd-microcode-is-unmeasured-on-fixed-firmware-and-through-cc6.md`).
//!
//! INIT keeps an Intel update and a hard reset clears it (§12.11.6.1), and
//! AMD's level resets to 0 at a warm or cold reset (55922 §1.4.4.11): a
//! resume path, when one exists, loads again.

use core::arch::{asm, global_asm};
use core::fmt;
use core::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

use kernel::microcode::{may_load, Loader, Patch, Revision, Unloaded};
use toyos_abi::boot::KernelArgs;
use toyos_microcode::PlatformId;

use super::cpu;
use super::idt::{IdtEntry, IdtPointer};
use crate::log;
use crate::mm::DirectMap;

/// `IA32_PLATFORM_ID`.
const PLATFORM_ID: u32 = 0x17;
/// `IA32_BIOS_UPDT_TRIG`.
const TRIGGER: u32 = 0x79;
/// Intel's `IA32_BIOS_SIGN_ID` and AMD's `PATCH_LEVEL`.
const SIGN_ID: u32 = 0x8b;
/// AMD's `PATCH_LOADER`.
const PATCH_LOADER: u32 = 0xc001_0020;
/// `#GP`'s vector.
const GP: usize = 13;

/// The patch the loader handed, as [`init`] published it before any CPU
/// loads; null where it handed none.
static PATCH: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
static PATCH_LEN: AtomicUsize = AtomicUsize::new(0);

/// The revision every CPU runs, as the first to read one published it.
static MACHINE: AtomicU64 = AtomicU64::new(UNPUBLISHED);
/// Outside a dword, so that [`Revision::DISABLED`] is a revision like another.
const UNPUBLISHED: u64 = u64::MAX;

/// Take the patch out of the handoff, on the BSP before its own load. The
/// loader's pages, which `kernel_main` keeps out of the allocator.
pub fn init(args: &KernelArgs) {
    let (at, len) = (args.microcode_addr, args.microcode_len);
    if len == 0 {
        return;
    }
    // The loader's contract, which Intel's trigger depends on: the update's
    // data, 48 bytes in, 16-byte aligned (SDM Vol. 3A §12.11.6).
    assert!(at.is_multiple_of(4096), "boot: the loader handed the microcode patch at {at:#x}, which is not page-aligned");
    let len = usize::try_from(len).expect("a length the loader allocated");
    // SAFETY: `at`+`len` is the loader's own `LoaderData` allocation, which
    // `kernel_main` checked and keeps out of the allocator, mapped at
    // `PHYS_OFFSET` through the boot map and the kernel's own; nothing else
    // names it, and only this, before any CPU loads, writes it.
    let patch = unsafe { core::slice::from_raw_parts_mut(DirectMap::from_phys(at).as_mut_ptr::<u8>(), len) };
    controls::apply(patch);
    PATCH_LEN.store(len, Ordering::Relaxed);
    PATCH.store(patch.as_mut_ptr(), Ordering::Release);
}

fn patch() -> Option<&'static [u8]> {
    let at = PATCH.load(Ordering::Acquire);
    // SAFETY: published by `init` from the slice it formed, kept and never
    // written again.
    (!at.is_null()).then(|| unsafe { core::slice::from_raw_parts(at, PATCH_LEN.load(Ordering::Relaxed)) })
}

/// Loads this CPU's microcode, says so, and holds it to the machine's.
pub fn load(cpu_id: u32) {
    let vendor = cpu::vendor();
    let (eax, _, ecx, _) = cpu::cpuid(1, 0);
    let Some(loader) = Loader::of(vendor) else {
        log!("microcode: cpu{cpu_id} {vendor:?} signature {eax:#x} loads nothing: ToyOS has no {vendor:?} microcode loader");
        return;
    };
    let found = revision(loader);
    let (platform, outcome) = match may_load(loader, ecx, found) {
        Err(why) => (None, Outcome::Unloaded(why)),
        Ok(()) => {
            let platform = match loader {
                Loader::Intel => Some(PlatformId::from_msr(cpu::rdmsr(PLATFORM_ID))),
                Loader::Amd => None,
            };
            let outcome = match patch().ok_or(Unloaded::NoPatch).and_then(|bytes| Patch::read(loader, bytes)) {
                Err(why) => Outcome::Unloaded(why),
                Ok(patch) if !patch.handed(loader, found) => Outcome::Current(patch.revision()),
                Ok(patch) => write(loader, &patch),
            };
            (platform, outcome)
        }
    };
    let running = match outcome {
        Outcome::Loaded(loaded) => loaded,
        Outcome::Refused { running, .. } => running,
        Outcome::Unloaded(_) | Outcome::Current(_) => found,
    };
    let platform = Platform(platform);
    log!("microcode: cpu{cpu_id} {vendor:?} signature {eax:#x}{platform} found {found:#x} {outcome} running {running:#x}");
    agree(cpu_id, running);
}

/// What one CPU did with the patch.
enum Outcome {
    Unloaded(Unloaded),
    /// The patch's revision, which the CPU is not handed: it runs that one or
    /// one above.
    Current(Revision),
    /// The update it loaded and read back.
    Loaded(Revision),
    /// The update it was handed and did not take, the error code of the
    /// `#GP` it raised if it raised one, and what it reads back.
    Refused { update: Revision, fault: Option<u64>, running: Revision },
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unloaded(why) => write!(f, "loads nothing: {why}"),
            Self::Current(newest) => write!(f, "current {newest:#x}"),
            Self::Loaded(loaded) => write!(f, "loaded {loaded:#x}"),
            Self::Refused { update, fault: None, .. } => write!(f, "refused {update:#x}"),
            Self::Refused { update, fault: Some(code), .. } => write!(f, "refused {update:#x} with #GP({code:#x})"),
        }
    }
}

/// A CPU's platform, where it was read.
struct Platform(Option<PlatformId>);

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(platform) => write!(f, " platform {platform}"),
            None => Ok(()),
        }
    }
}

/// The revision this CPU runs.
fn revision(loader: Loader) -> Revision {
    match loader {
        Loader::Intel => {
            // SAFETY: writing `IA32_BIOS_SIGN_ID` 0 is §12.11.7.1's own first
            // step, and loads nothing.
            unsafe { cpu::wrmsr(SIGN_ID, 0) };
            cpu::cpuid(1, 0);
            Revision::from_sign_id(cpu::rdmsr(SIGN_ID))
        }
        // Read alone: `PATCH_LEVEL` is error-on-write (55922 §2.1.14.1).
        Loader::Amd => Revision::from_patch_level(cpu::rdmsr(SIGN_ID)),
    }
}

/// Hands this CPU `patch` and judges what it made of it.
fn write(loader: Loader, patch: &Patch<'static>) -> Outcome {
    match loader {
        Loader::Intel => {
            // SAFETY: an Intel CPU, and an update's data, which `init`'s page
            // alignment starts 16-byte aligned 48 bytes in, mapped present at
            // a canonical address (§12.11.6).
            unsafe { trigger(patch.trigger().as_ptr()) };
            taken(loader, patch.revision())
        }
        Loader::Amd => match patch_loader(patch.trigger()) {
            Ok(()) => taken(loader, patch.revision()),
            Err(code) => Outcome::Refused { update: patch.revision(), fault: Some(code), running: revision(loader) },
        },
    }
}

/// `IA32_BIOS_UPDT_TRIG` written the address of the data the CPU loads. Not
/// `cpu::wrmsr`, which is `nomem`: the CPU reads memory through this write.
///
/// # Safety
/// The CPU is Intel's, and `at` the start of an update's data, placed as
/// §12.11.6 asks.
unsafe fn trigger(at: *const u8) {
    let at = at as u64;
    // SAFETY: the caller's.
    unsafe {
        asm!("wrmsr", in("ecx") TRIGGER, in("eax") at as u32, in("edx") (at >> 32) as u32, options(nostack));
    }
}

/// `PATCH_LOADER` written the linear address of `block`, an MPB the CPU
/// reads whole, or the error code of the `#GP` the write raised.
///
/// The write runs with interrupts masked under a table whose one gate is
/// [`amd_patch_load_gp`]'s, and the table it found is loaded again after.
fn patch_loader(block: &'static [u8]) -> Result<(), u64> {
    let cs: u16;
    // SAFETY: reads `CS` alone.
    unsafe { asm!("mov {:x}, cs", out(reg) cs, options(nomem, nostack, preserves_flags)) };
    let mut gates = [IdtEntry::EMPTY; GP + 1];
    gates[GP] = IdtEntry::gate(amd_patch_load_gp as *const () as u64, cs);
    let table = IdtPointer { limit: (size_of_val(&gates) - 1) as u16, base: gates.as_ptr() as u64 };
    // SAFETY: `block` is the loader's patch, kept and present for as long as
    // the CPU reads it; `table` holds a gate onto the routine's own `#GP`
    // handler in this CPU's code segment and nothing else.
    match unsafe { amd_patch_load(block.as_ptr(), &table) } {
        0 => Ok(()),
        faulted => Err(faulted & !FAULTED),
    }
}

/// Set in [`amd_patch_load`]'s answer when the write raised `#GP`, whose
/// error code is the rest.
const FAULTED: u64 = 1 << 63;

extern "sysv64" {
    /// Writes `PATCH_LOADER` the address `block` with interrupts masked and
    /// `table` loaded, then loads the table it found and restores `RFLAGS`.
    /// Answers 0, or [`FAULTED`] and the error code of the `#GP` the write
    /// raised.
    fn amd_patch_load(block: *const u8, table: *const IdtPointer) -> u64;
    /// `amd_patch_load`'s `#GP` gate: resumes past the write, holding the
    /// error code. A `#GP` anywhere else, or one an event's delivery raised
    /// (error code bit 0 or 1), ends in `ud2`. The table holds no other gate:
    /// anything else in the window, an NMI or machine check among it, resets
    /// the machine.
    fn amd_patch_load_gp();
}

global_asm!(
    ".p2align 4",
    ".globl amd_patch_load",
    ".hidden amd_patch_load",
    "amd_patch_load:",
    "pushfq",
    "cli",
    "sub rsp, 16",
    "sidt [rsp]",
    "lidt [rsi]",
    "mov ecx, {patch_loader}",
    "mov eax, edi",
    "mov rdx, rdi",
    "shr rdx, 32",
    "xor r8d, r8d",
    ".Lamd_patch_load_write:",
    "wrmsr",
    ".Lamd_patch_load_resume:",
    "lidt [rsp]",
    "add rsp, 16",
    "popfq",
    "mov rax, r8",
    "ret",
    "",
    ".p2align 4",
    ".globl amd_patch_load_gp",
    ".hidden amd_patch_load_gp",
    "amd_patch_load_gp:",
    "lea r9, [rip + .Lamd_patch_load_write]",
    "cmp [rsp + 8], r9",
    "jne 2f",
    "test qword ptr [rsp], 3",
    "jnz 2f",
    "pop r8",
    "bts r8, 63",
    "lea r9, [rip + .Lamd_patch_load_resume]",
    "mov [rsp], r9",
    "iretq",
    "2:",
    "ud2",
    patch_loader = const PATCH_LOADER,
);

/// What the CPU made of the update of revision `update` it was handed: the
/// revision it reads back, which it runs.
fn taken(loader: Loader, update: Revision) -> Outcome {
    match revision(loader) {
        running if running == update => Outcome::Loaded(update),
        running => Outcome::Refused { update, fault: None, running },
    }
}

/// Every CPU that reads a revision runs the first one's.
fn agree(cpu_id: u32, running: Revision) {
    let mine = u64::from(running.0);
    if let Err(machine) = MACHINE.compare_exchange(UNPUBLISHED, mine, Ordering::Relaxed, Ordering::Relaxed) {
        assert!(
            machine == mine,
            "microcode: cpu{cpu_id} runs {running:#x} and the first CPU {machine:#x}: this machine's \
             CPUs do not run one revision",
        );
    }
}

/// The negative controls on the read-back and on AMD's guard: each changes
/// the patch every CPU is handed, whatever its vendor.
#[cfg(feature = "boot-actuators")]
mod controls {
    pub fn apply(patch: &mut [u8]) {
        if crate::actuator::microcode_reseal() {
            // An Intel header's revision (dword 1) raised by one and its
            // checksum (dword 4) lowered by one, so the header and data still
            // sum to 0 (SDM Vol. 3A §12.11.5) while Intel's payload says the
            // revision it did.
            let word = |patch: &mut [u8], at: usize, by: fn(u32) -> u32| {
                let bytes = patch.get_mut(at..at + 4).expect("an Intel update's header");
                let value = by(u32::from_le_bytes(bytes.try_into().expect("four bytes")));
                bytes.copy_from_slice(&value.to_le_bytes());
            };
            word(patch, 4, |revision| revision.wrapping_add(1));
            word(patch, 16, |checksum| checksum.wrapping_sub(1));
        }
        if crate::actuator::microcode_amd_flip() {
            // The first byte of an AMD MPB's patch data, behind its 32-byte
            // header.
            *patch.get_mut(32).expect("an AMD MPB's data") ^= 1;
        }
    }
}

#[cfg(not(feature = "boot-actuators"))]
mod controls {
    pub fn apply(_: &mut [u8]) {}
}
