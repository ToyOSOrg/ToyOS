//! This CPU's microcode, loaded from the file `toyos_microcode` embeds for
//! its vendor before anything reads what an update changes: Intel asks it
//! "on each core as early as possible in the OS boot sequence, before any
//! CPUID feature flags or other enumeration values are cached". The BSP
//! loads in `boot::after_console`, before `random::key` draws on RDRAND and
//! RDSEED and before `control_regs::init` reads CPUID; each AP in
//! `smp::ap_entry`, before its own. `boot_aps` starts one AP at a time, the
//! serialisation per core SDM Vol. 3A §12.11.6.3 asks.
//!
//! **Both run before this kernel's IDT**, so a fault inside Intel's
//! trigger reports nothing: the BSP's vectors into firmware's table, an AP's
//! into the kernel's with `CR4.OSFXSR` still clear, a triple fault. Intel
//! documents no fault for a valid update at a present, canonical address.
//! AMD documents a `#GP` for a patch the CPU refuses (57896 §2.1.3.2), so
//! AMD's write runs under a table of its own whose one gate catches that
//! write's `#GP` and nothing else.
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
//! One line per CPU says what it found, what the file holds for it and what
//! it runs; a CPU that loads nothing says why, and a CPU of a vendor ToyOS
//! has no loader for says so by the vendor's name and reads nothing more.
//! Every CPU that reads a revision runs the one the first did, or the boot
//! stops naming it: whatever reads the revision assumes one.
//!
//! **AMD documents its patch loader for Families 19h and 1Ah alone** (55901
//! §2.1.15.3, 57896 §2.1.13.3). On the AMD bench, Family 17h Model 60h on
//! firmware without AMD-SB-7033's fix, ToyOSOrg/evidence `32c335ece`
//! (`metal/amd-bench/`) measured what no Family 17h document states: writing
//! a patch's linear address to MSRC001_0020 loads it, in place in the
//! container 0xC past a 16-byte boundary; a refused patch raises `#GP(0)`
//! and leaves the level; and a write at the level a core runs, its second
//! thread's after its first loaded among them, changes nothing and does not
//! fault. Unmeasured: firmware with that fix, which may refuse every load and
//! whose CPUs then say `refused`; and whether a core keeps its patch through
//! CC6.
//!
//! INIT keeps an Intel update and a hard reset clears it (§12.11.6.1), and
//! AMD's level resets to 0 at a warm or cold reset (55922 §1.4.4.11): a
//! resume path, when one exists, loads again.

use core::arch::{asm, global_asm};
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use toyos_microcode::{amd, Choice, Cpu, Loader, PlatformId, Revision, Signature, Unloaded};

use super::cpu;
use super::idt::{IdtEntry, IdtPointer};
use crate::log;

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

/// The revision every CPU runs, as the first to read one published it.
static MACHINE: AtomicU64 = AtomicU64::new(UNPUBLISHED);
/// Outside a dword, so that [`Revision::DISABLED`] is a revision like another.
const UNPUBLISHED: u64 = u64::MAX;

/// Loads this CPU's microcode, says so, and holds it to the machine's.
pub fn load(cpu_id: u32) {
    let vendor = cpu::vendor();
    let (eax, _, ecx, _) = cpu::cpuid(1, 0);
    let loader = match toyos_microcode::loader(vendor) {
        Ok(loader) => loader,
        Err(why) => {
            log!("microcode: cpu{cpu_id} {vendor:?} signature {eax:#x} loads nothing: {why}");
            return;
        }
    };
    let found = revision(loader);
    let (platform, outcome) = match (toyos_microcode::may_load(loader, ecx, found), loader) {
        (Err(why), _) => (None, Outcome::Unloaded(why)),
        (Ok(()), Loader::Intel) => {
            let platform = PlatformId::from_msr(cpu::rdmsr(PLATFORM_ID));
            let cpu = Cpu { signature: Signature(eax), platform, revision: found };
            (Some(platform), load_intel(cpu_id, &cpu))
        }
        (Ok(()), Loader::Amd) => (None, load_amd(&amd::Cpu { signature: Signature(eax), level: found })),
    };
    let running = match outcome {
        Outcome::Loaded(loaded) => loaded,
        Outcome::Refused { running, .. } => running,
        Outcome::Unloaded(_) | Outcome::NoMatch | Outcome::Current(_) => found,
    };
    let platform = Platform(platform);
    log!("microcode: cpu{cpu_id} {vendor:?} signature {eax:#x}{platform} found {found:#x} {outcome} running {running:#x}");
    agree(cpu_id, running);
}

/// What one CPU did with the embedded file.
enum Outcome {
    Unloaded(Unloaded),
    NoMatch,
    /// The newest update naming it, which it runs, or runs one above.
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
            Self::NoMatch => f.write_str("loads nothing: no embedded update names it"),
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

fn load_intel(cpu_id: u32, cpu: &Cpu) -> Outcome {
    let choice = toyos_microcode::select(reseal::intel_file(cpu_id), cpu).unwrap_or_else(|refused| {
        panic!("microcode: the embedded Intel file is refused at byte {}: {:?}", refused.at, refused.why)
    });
    match choice {
        Choice::NoMatch => Outcome::NoMatch,
        Choice::Current(newest) => Outcome::Current(newest),
        Choice::Load(update) => {
            // SAFETY: a validated update's data, which `INTEL`'s alignment and
            // every update's 1024-byte granule start 16-byte aligned, inside
            // the kernel image and so canonical and present (§12.11.6).
            unsafe { trigger(update.data().as_ptr()) };
            taken(Loader::Intel, update.revision())
        }
    }
}

fn load_amd(cpu: &amd::Cpu) -> Outcome {
    let choice = amd::select(corrupt::amd_file(), cpu).unwrap_or_else(|refused| {
        panic!("microcode: the embedded AMD file is refused at byte {}: {:?}", refused.at, refused.why)
    });
    match choice {
        Choice::NoMatch => Outcome::NoMatch,
        Choice::Current(newest) => Outcome::Current(newest),
        Choice::Load(patch) => match patch_loader(patch.block()) {
            Ok(()) => taken(Loader::Amd, patch.level()),
            Err(code) => Outcome::Refused { update: patch.level(), fault: Some(code), running: revision(Loader::Amd) },
        },
    }
}

/// `IA32_BIOS_UPDT_TRIG` written the address of the data the CPU loads. Not
/// `cpu::wrmsr`, which is `nomem`: the CPU reads memory through this write.
///
/// # Safety
/// The CPU is Intel's, and `at` the start of a validated update's data,
/// placed as §12.11.6 asks.
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
    // SAFETY: `block` is a validated MPB in the kernel image, present for
    // as long as the CPU reads it; `table` holds a gate onto the routine's
    // own `#GP` handler in this CPU's code segment and nothing else.
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
    /// error code. A fault anywhere else, or one an event's delivery raised
    /// (error code bit 0 or 1), is no refusal and ends in `ud2`, which this
    /// table has no gate for: a triple fault.
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

/// `microcode-reseal`: the BSP is handed the T14's update with its header's
/// revision raised by one and its checksum sealed again, while Intel's payload
/// still says 0xbe, so its line must say `refused`.
#[cfg(feature = "boot-actuators")]
mod reseal {
    use toyos_microcode::Aligned;

    const LEN: usize = 112_640;

    static RAISED: Aligned<[u8; LEN]> =
        Aligned(raised(*include_bytes!("../../../microcode/intel-ucode/06-8c-01")));

    /// Table 12-7's header: the revision at 4, the checksum at 16, the data
    /// size at 28; the header and data sum to 0 (§12.11.5).
    const fn raised(mut file: [u8; LEN]) -> [u8; LEN] {
        file = put(file, 4, word(&file, 4) + 1);
        file = put(file, 16, 0);
        let end = 48 + word(&file, 28) as usize;
        let (mut sum, mut at) = (0u32, 0);
        while at < end {
            sum = sum.wrapping_add(word(&file, at));
            at += 4;
        }
        put(file, 16, sum.wrapping_neg())
    }

    const fn word(file: &[u8; LEN], at: usize) -> u32 {
        u32::from_le_bytes([file[at], file[at + 1], file[at + 2], file[at + 3]])
    }

    const fn put(mut file: [u8; LEN], at: usize, value: u32) -> [u8; LEN] {
        let bytes = value.to_le_bytes();
        let mut i = 0;
        while i < 4 {
            file[at + i] = bytes[i];
            i += 1;
        }
        file
    }

    pub fn intel_file(cpu_id: u32) -> &'static [u8] {
        if crate::actuator::microcode_reseal() && cpu_id == 0 {
            &RAISED.0
        } else {
            &toyos_microcode::INTEL.0
        }
    }
}

#[cfg(not(feature = "boot-actuators"))]
mod reseal {
    pub fn intel_file(_cpu_id: u32) -> &'static [u8] {
        &toyos_microcode::INTEL.0
    }
}

/// `microcode-amd-flip`: every CPU is handed AMD's container with the first
/// byte of the AMD bench's patch data flipped, which its CPUs refuse with
/// `#GP`, so each line must say `refused` and the boot go on.
#[cfg(feature = "boot-actuators")]
mod corrupt {
    use toyos_microcode::Aligned;

    const LEN: usize = 22_596;
    /// The bench's MPB starts at 0x262c in the pinned file, its patch data
    /// 32 bytes in (`toyos_microcode::amd`'s tests hold the first).
    const FLIPPED: usize = 0x262c + 32;

    static FLIPPED_FILE: Aligned<[u8; LEN]> = Aligned(flipped(*include_bytes!(
        "../../../microcode/amd-ucode/microcode_amd_fam17h.bin"
    )));

    const fn flipped(mut file: [u8; LEN]) -> [u8; LEN] {
        file[FLIPPED] ^= 1;
        file
    }

    pub fn amd_file() -> &'static [u8] {
        if crate::actuator::microcode_amd_flip() {
            &FLIPPED_FILE.0
        } else {
            &toyos_microcode::AMD.0
        }
    }
}

#[cfg(not(feature = "boot-actuators"))]
mod corrupt {
    pub fn amd_file() -> &'static [u8] {
        &toyos_microcode::AMD.0
    }
}
