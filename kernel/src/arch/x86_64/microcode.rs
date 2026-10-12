//! This CPU's microcode, loaded from the files `toyos_microcode` embeds
//! before anything reads what an update changes: Intel asks it "on each core
//! as early as possible in the OS boot sequence, before any CPUID feature
//! flags or other enumeration values are cached". The BSP loads in
//! `boot::after_console`, before `random::key` draws on RDRAND and RDSEED and
//! before `control_regs::init` reads CPUID; each AP in `smp::ap_entry`, before
//! its own. `boot_aps` starts one AP at a time, the serialisation per core SDM
//! Vol. 3A §12.11.6.3 asks.
//!
//! **Both run before this kernel's IDT**, so a fault inside a trigger reports
//! nothing: the BSP's vectors into firmware's table, an AP's into the kernel's
//! with `CR4.OSFXSR` still clear, a triple fault. Neither maker documents a
//! fault for a valid update at a present, canonical address.
//!
//! **A CPU's verdict on an update is the revision it reads back, and a
//! refusal is reported, not a panic.** Intel judges a load by MSR 8BH read
//! after it, equal to the update's revision or a failure (SDM Vol. 3A
//! §12.11.7.2, Example 12-10), and the register reports "the currently
//! functioning revision" (§12.11.7.1). AMD's PPR for family 17h model 60h
//! (55922 rev 3.06, MSR0000_008B) has it return "an identification number for
//! the microcode patch that has been loaded". So a CPU that does not take an
//! update runs the revision it reads back: its line says `refused` with the
//! update it was handed, its `running` is that read-back, and it is held to
//! the machine's like any other.
//!
//! One line per CPU says what it found, what the file holds for it and what
//! it runs; a CPU that loads nothing says why. Every CPU that reads a revision
//! runs the one the first did, or the boot stops naming it: whatever reads the
//! revision assumes one.
//!
//! INIT keeps an update and a hard reset clears it (§12.11.6.1): a resume
//! path, when one exists, loads again.

use core::arch::asm;
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use toyos_microcode::{amd, intel, Choice, Maker, Revision, Signature, Unloaded};

use super::cpu;
use crate::log;

/// `IA32_PLATFORM_ID`.
const PLATFORM_ID: u32 = 0x17;
/// `IA32_BIOS_UPDT_TRIG`.
const INTEL_TRIGGER: u32 = 0x79;
/// `IA32_BIOS_SIGN_ID`; AMD's `PATCH_LEVEL` (55922, MSR0000_008B).
const SIGN_ID: u32 = 0x8b;
/// AMD's patch loader, which no AMD document in hand describes.
const AMD_TRIGGER: u32 = 0xc001_0020;

/// The revision every CPU runs, as the first to read one published it.
static MACHINE: AtomicU64 = AtomicU64::new(UNPUBLISHED);
/// Outside a dword, so that [`Revision::DISABLED`] is a revision like another.
const UNPUBLISHED: u64 = u64::MAX;

/// Loads this CPU's microcode, says so, and holds it to the machine's.
pub fn load(cpu_id: u32) {
    let vendor = cpu::vendor();
    let (eax, _, ecx, _) = cpu::cpuid(1, 0);
    let signature = Signature(eax);
    let maker = match Maker::of(vendor) {
        Ok(maker) => maker,
        Err(why) => {
            log!("microcode: cpu{cpu_id} {vendor:?} signature {eax:#x} loads nothing: {why}");
            return;
        }
    };
    let found = revision(maker);
    let (platform, outcome) = match toyos_microcode::may_load(ecx, found) {
        Err(why) => (None, Outcome::Unloaded(why)),
        Ok(()) => match maker {
            Maker::Intel => {
                let platform = intel::PlatformId::from_msr(cpu::rdmsr(PLATFORM_ID));
                let cpu = intel::Cpu { signature, platform, revision: found };
                (Some(platform), load_intel(cpu_id, &cpu))
            }
            Maker::Amd => (None, load_amd(signature, found)),
        },
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

/// What one CPU did with the file its maker's is.
enum Outcome {
    Unloaded(Unloaded),
    NoMatch,
    /// The newest update naming it, which it runs, or runs one above.
    Current(Revision),
    /// The update it loaded and read back.
    Loaded(Revision),
    /// The update it was handed and did not take, and what it reads back.
    Refused { update: Revision, running: Revision },
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unloaded(why) => write!(f, "loads nothing: {why}"),
            Self::NoMatch => f.write_str("loads nothing: no embedded update names it"),
            Self::Current(newest) => write!(f, "current {newest:#x}"),
            Self::Loaded(loaded) => write!(f, "loaded {loaded:#x}"),
            Self::Refused { update, .. } => write!(f, "refused {update:#x}"),
        }
    }
}

/// An Intel CPU's platform, where it was read.
struct Platform(Option<intel::PlatformId>);

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(platform) => write!(f, " platform {platform}"),
            None => Ok(()),
        }
    }
}

/// The revision this CPU runs, read as its maker reports it.
fn revision(maker: Maker) -> Revision {
    match maker {
        Maker::Intel => {
            // SAFETY: writing `IA32_BIOS_SIGN_ID` 0 is §12.11.7.1's own first
            // step, and loads nothing.
            unsafe { cpu::wrmsr(SIGN_ID, 0) };
            cpu::cpuid(1, 0);
            Revision::from_sign_id(cpu::rdmsr(SIGN_ID))
        }
        Maker::Amd => Revision::from_patch_level(cpu::rdmsr(SIGN_ID)),
    }
}

fn load_intel(cpu_id: u32, cpu: &intel::Cpu) -> Outcome {
    let choice = intel::select(reseal::intel_file(cpu_id), cpu).unwrap_or_else(|refused| {
        panic!("microcode: the embedded Intel file is refused at byte {}: {:?}", refused.at, refused.why)
    });
    match choice {
        Choice::NoMatch => Outcome::NoMatch,
        Choice::Current(newest) => Outcome::Current(newest),
        Choice::Load(update) => {
            // SAFETY: a validated update's data, which `INTEL`'s alignment and
            // every update's 1024-byte granule start 16-byte aligned, inside
            // the kernel image and so canonical and present (§12.11.6).
            unsafe { trigger(INTEL_TRIGGER, update.data().as_ptr()) };
            taken(Maker::Intel, update.revision())
        }
    }
}

fn load_amd(signature: Signature, found: Revision) -> Outcome {
    let choice = amd::select(&toyos_microcode::AMD.0, signature, found).unwrap_or_else(|refused| {
        panic!("microcode: the embedded AMD container is refused at byte {}: {:?}", refused.at, refused.why)
    });
    match choice {
        Choice::NoMatch => Outcome::NoMatch,
        Choice::Current(newest) => Outcome::Current(newest),
        Choice::Load(patch) => {
            let bytes = patch.bytes().as_ptr_range();
            // SAFETY: a validated patch inside the kernel image, so its
            // address is canonical and present.
            unsafe { trigger(AMD_TRIGGER, bytes.start) };
            if patch.leaves_its_mapping_uncached() {
                cpu::invlpg(bytes.start as u64);
                cpu::invlpg(bytes.end as u64 - 1);
            }
            taken(Maker::Amd, patch.revision())
        }
    }
}

/// A trigger MSR written the address of the bytes the CPU loads from. Not
/// `cpu::wrmsr`, which is `nomem`: the CPU reads memory through this write.
///
/// # Safety
/// `msr` is a maker's trigger on a CPU of that maker, and `at` the start of
/// one of that maker's validated updates, placed as its trigger asks.
unsafe fn trigger(msr: u32, at: *const u8) {
    let at = at as u64;
    // SAFETY: the caller's.
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") at as u32, in("edx") (at >> 32) as u32, options(nostack));
    }
}

/// What the CPU made of the update of revision `update` it was handed: the
/// revision it reads back, which it runs.
fn taken(maker: Maker, update: Revision) -> Outcome {
    match revision(maker) {
        running if running == update => Outcome::Loaded(update),
        running => Outcome::Refused { update, running },
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
