//! The Power State Coordination Interface (Arm DEN0022), through the conduit
//! the FADT's `ARM_BOOT_ARCH` names (ACPI 6.5 §5.2.9): `SMC` to the secure monitor, or `HVC`
//! where a hypervisor stands in for it. Every call is SMCCC's (Arm DEN0028D):
//! the function in `x0`, its arguments in `x1`–`x3`, the answer in `x0`.

use core::sync::atomic::{AtomicU8, Ordering::Relaxed};

use toyos_acpi::Psci;

use crate::drivers::acpi::direct_phys;
use crate::log;

/// The function IDs this kernel calls, as PSCI numbers them from 0.2 on
/// (DEN0022 §5.1), each the SMC64 one where there is one.
const PSCI_VERSION: u32 = 0x8400_0000;
const CPU_OFF: u32 = 0x8400_0002;
const CPU_ON: u32 = 0xC400_0003;
const AFFINITY_INFO: u32 = 0xC400_0004;
const SYSTEM_OFF: u32 = 0x8400_0008;
const SYSTEM_RESET: u32 = 0x8400_0009;

/// `CPU_ON`'s `target_cpu` and `AFFINITY_INFO`'s `target_affinity`:
/// `MPIDR_EL1`'s affinity fields where they stand, Aff3 in bits 39:32, every
/// other bit zero.
const TARGET_CPU: u64 = 0xFF_00FF_FFFF;

/// How this machine reaches PSCI.
#[derive(Clone, Copy)]
#[repr(u8)]
pub enum Conduit {
    Smc = 1,
    Hvc,
}

/// The conduit [`init`] found, zero for none: one word, because a reset on a
/// wedged machine reads it and may take no lock. Written on the boot CPU
/// before any other starts.
static CONDUIT: AtomicU8 = AtomicU8::new(0);

/// What `AFFINITY_INFO` answers of one CPU (DEN0022 §5.7.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Affinity {
    On,
    Off,
    OnPending,
}

/// A PSCI call's refusal, as the specification numbers its return codes.
#[derive(Clone, Copy, Debug)]
pub enum Error {
    NotSupported,
    InvalidParameters,
    Denied,
    AlreadyOn,
    OnPending,
    InternalFailure,
    NotPresent,
    Disabled,
    InvalidAddress,
    Unnamed(i32),
}

impl Error {
    fn of(code: i32) -> Self {
        match code {
            -1 => Self::NotSupported,
            -2 => Self::InvalidParameters,
            -3 => Self::Denied,
            -4 => Self::AlreadyOn,
            -5 => Self::OnPending,
            -6 => Self::InternalFailure,
            -7 => Self::NotPresent,
            -8 => Self::Disabled,
            -9 => Self::InvalidAddress,
            other => Self::Unnamed(other),
        }
    }
}

impl Conduit {
    /// `function` with three arguments; the answer's low 32 bits, which is
    /// where every function this kernel calls puts it. `x4`–`x17` are the
    /// callee's to clobber under SMCCC 1.0, so they are declared so.
    fn call(self, function: u32, a1: u64, a2: u64, a3: u64) -> i32 {
        let answer: u64;
        macro_rules! smccc {
            ($instruction:literal) => {
                core::arch::asm!(
                    $instruction,
                    inout("x0") u64::from(function) => answer,
                    inout("x1") a1 => _, inout("x2") a2 => _, inout("x3") a3 => _,
                    out("x4") _, out("x5") _, out("x6") _, out("x7") _, out("x8") _, out("x9") _,
                    out("x10") _, out("x11") _, out("x12") _, out("x13") _, out("x14") _,
                    out("x15") _, out("x16") _, out("x17") _,
                    options(nostack),
                )
            };
        }
        // SAFETY: an SMCCC call into firmware or a hypervisor, which returns
        // to the next instruction; not `nomem`, since a callee may read what
        // this CPU stored before it.
        unsafe {
            match self {
                Self::Smc => smccc!("smc #0"),
                Self::Hvc => smccc!("hvc #0"),
            }
        }
        answer as i32
    }

    /// Start the CPU whose `MPIDR_EL1` is `mpidr` at the physical address
    /// `entry`, with `context` in its `x0`, at the highest exception level
    /// this CPU's firmware gives an operating system.
    pub fn cpu_on(self, mpidr: u64, entry: u64, context: u64) -> Result<(), Error> {
        match self.call(CPU_ON, mpidr & TARGET_CPU, entry, context) {
            0 => Ok(()),
            code => Err(Error::of(code)),
        }
    }

    /// Turn this CPU off: an answer is a refusal, since `CPU_OFF` does not
    /// return when it succeeds.
    pub fn cpu_off(self) -> Error {
        Error::of(self.call(CPU_OFF, 0, 0, 0))
    }

    /// Whether the CPU whose `MPIDR_EL1` is `mpidr` is on, off, or on its way
    /// on: affinity level 0, the CPU itself.
    pub fn affinity_info(self, mpidr: u64) -> Result<Affinity, Error> {
        match self.call(AFFINITY_INFO, mpidr & TARGET_CPU, 0, 0) {
            0 => Ok(Affinity::On),
            1 => Ok(Affinity::Off),
            2 => Ok(Affinity::OnPending),
            code => Err(Error::of(code)),
        }
    }

    /// Power the machine off. Neither this nor [`Conduit::system_reset`]
    /// returns (DEN0022 §5.1.9, §5.1.11), except where firmware breaks that.
    pub fn system_off(self) {
        self.call(SYSTEM_OFF, 0, 0, 0);
    }

    /// Reset the machine cold.
    pub fn system_reset(self) {
        self.call(SYSTEM_RESET, 0, 0, 0);
    }

    fn name(self) -> &'static str {
        match self {
            Self::Smc => "SMC",
            Self::Hvc => "HVC",
        }
    }
}

/// PSCI as the FADT names it, asked for its version and kept for
/// [`conduit`]; none where the machine offers none this kernel can call,
/// which the log says.
pub fn init(rsdp_addr: u64) {
    let fadt = match toyos_acpi::find_table(direct_phys(), rsdp_addr, b"FACP", toyos_acpi::SDT_HEADER_LEN) {
        Ok(fadt) => fadt,
        Err(e) => {
            log!("PSCI: none, because the FADT is unusable: {e:?}");
            return;
        }
    };
    let conduit = match toyos_acpi::psci(&fadt) {
        Psci::Smc => Conduit::Smc,
        Psci::Hvc => Conduit::Hvc,
        Psci::Absent => {
            log!("PSCI: none: the FADT's ARM_BOOT_ARCH does not set PSCI_COMPLIANT");
            return;
        }
        Psci::Undefined { revision, minor } => {
            log!("PSCI: none: the FADT is version {revision}.{minor}, and ARM_BOOT_ARCH begins at 5.1");
            return;
        }
        Psci::Short => {
            log!("PSCI: none: the FADT ends before ARM_BOOT_ARCH and the minor version after it");
            return;
        }
    };
    let version = conduit.call(PSCI_VERSION, 0, 0, 0);
    if version < 0 {
        log!("PSCI: PSCI_VERSION through {} answered {:?}; none used", conduit.name(), Error::of(version));
        return;
    }
    let (major, minor) = (version >> 16, version & 0xFFFF);
    if major == 0 && minor < 2 {
        log!("PSCI: {major}.{minor} through {}, which numbers no SMC64 CPU_ON; none used", conduit.name());
        return;
    }
    log!("PSCI: {major}.{minor} through {}", conduit.name());
    CONDUIT.store(conduit as u8, Relaxed);
}

/// The conduit [`init`] kept, or `None` on a machine without PSCI.
pub fn conduit() -> Option<Conduit> {
    match CONDUIT.load(Relaxed) {
        0 => None,
        1 => Some(Conduit::Smc),
        2 => Some(Conduit::Hvc),
        other => unreachable!("PSCI: the kept conduit reads {other}, which `init` never stores"),
    }
}
