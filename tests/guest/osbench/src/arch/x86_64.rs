//! What osbench needs written for x86-64 in particular: CPU identity and
//! topology as CPUID reads them (on ToyOS, which has no call that says, and
//! on both for the munmap pair's APIC IDs), a chain of dependent adds to time
//! the clock with, and the bytes of a program that exits.

pub use cpuid::{cpu_id, cpu_id_is};
#[cfg(target_os = "toyos")]
pub use cpuid::{core_of, cpu_model, current_cpu, threads_per_core};
#[cfg(target_os = "linux")]
pub use cpuid::power_registers;
#[cfg(target_os = "toyos")]
pub use cpuid::llc_of_apic;

/// `turns` turns of 100 dependent register adds, each waiting one cycle for
/// the one before it on every x86-64 core either bench has (Intel's and AMD's
/// optimization manuals list `ADD r64, r64` at one cycle). Inline assembly, so
/// the chain is the one written whatever the optimizer does.
pub fn add_chain(turns: u64) {
    let (mut x, mut n) = (std::hint::black_box(0u64), turns);
    // SAFETY: arithmetic on two registers this block owns.
    unsafe {
        std::arch::asm!(
            "2:",
            ".rept 100",
            "add {x}, 1",
            ".endr",
            "dec {n}",
            "jnz 2b",
            x = inout(reg) x,
            n = inout(reg) n,
            options(nomem, nostack),
        );
    }
    std::hint::black_box((x, n));
}

/// Adds per turn of [`add_chain`].
pub const ADDS_PER_TURN: u64 = 100;

/// The ELF machine a generated program is for (`EM_X86_64`).
pub const ELF_MACHINE: u16 = 62;

/// What a generated program runs, position-independent: `exit_group(0)` (the
/// x86-64 syscall table's 231, the number in RAX and the code in RDI), then a
/// trap that is never reached.
#[cfg(target_os = "linux")]
pub const EXIT_CODE: [u8; 11] = [
    0xb8, 0xe7, 0x00, 0x00, 0x00, // mov eax, 231 (exit_group)
    0x31, 0xff, // xor edi, edi
    0x0f, 0x05, // syscall
    0x0f, 0x0b, // ud2
];

#[cfg(target_os = "linux")]
const _: () = assert!(libc::SYS_exit_group == 231);

/// What a generated program runs, position-independent: `exit(0)` by
/// `SYS_EXIT` (`toyos_abi::syscall`: the number in RDI, the code in RSI), then
/// a trap that is never reached.
#[cfg(target_os = "toyos")]
pub const EXIT_CODE: [u8; 11] = [
    0xbf, 0x48, 0x00, 0x00, 0x00, // mov edi, 72 (SYS_EXIT)
    0x31, 0xf6, // xor esi, esi
    0x0f, 0x05, // syscall
    0x0f, 0x0b, // ud2
];

#[cfg(target_os = "toyos")]
const _: () = assert!(toyos_abi::syscall::SYS_EXIT == 0x48);

mod cpuid {
    use core::arch::x86_64::__cpuid_count;
    use std::sync::OnceLock;

    /// The registers the energy group reads on Linux for the counters ToyOS's
    /// kernel reads (`kernel/pure/cpu.rs`), by CPUID.0's vendor: Intel's from
    /// SDM Vol. 4 Table 2-20, AMD's from PPR 55922 on Family 17h with
    /// `CPUID_Fn80000007_EDX[RAPL]`, where they are 32-bit counters; another
    /// AMD family is refused, as the kernel refuses it. A register a CPU
    /// lacks reads as absent.
    #[cfg(target_os = "linux")]
    pub fn power_registers() -> Option<crate::sys::PowerRegisters> {
        let id = __cpuid_count(0, 0);
        let vendor: Vec<u8> = [id.ebx, id.edx, id.ecx].iter().flat_map(|r| r.to_le_bytes()).collect();
        match &vendor[..] {
            b"GenuineIntel" => Some(crate::sys::PowerRegisters {
                energy: 0x611,
                power_unit: 0x606,
                package_residency: [Some(0x60d), Some(0x3f8), Some(0x3f9), Some(0x3fa)],
                core_residency: [Some(0x3fd), Some(0x3fe)],
                core_energy: None,
            }),
            b"AuthenticAMD" if amd_family_17h_rapl() => Some(crate::sys::PowerRegisters {
                energy: 0xc001_029b,
                power_unit: 0xc001_0299,
                package_residency: [None; 4],
                core_residency: [None; 2],
                core_energy: Some(0xc001_029a),
            }),
            _ => None,
        }
    }

    /// Family 17h (CPUID.1:EAX base family 0FH plus extended family 08H) with
    /// RAPL, bit 14 of CPUID_Fn80000007_EDX (PPR 55922).
    #[cfg(target_os = "linux")]
    fn amd_family_17h_rapl() -> bool {
        let eax = __cpuid_count(1, 0).eax;
        let family = ((eax >> 8) & 0xf) + ((eax >> 20) & 0xff);
        let rapl = __cpuid_count(0x8000_0000, 0).eax >= 0x8000_0007 && __cpuid_count(0x8000_0007, 0).edx & (1 << 14) != 0;
        family == 0x17 && rapl
    }

    struct Topology {
        /// CPUID leaf 0BH enumerates topology (SDM Vol. 2A: a subleaf 0 whose
        /// EBX[15:0] is 0 enumerates nothing).
        x2apic: bool,
        /// Right shift from an APIC ID to its core's ID.
        #[cfg_attr(target_os = "linux", allow(dead_code))]
        smt_shift: u32,
    }

    fn topology() -> &'static Topology {
        static TOPOLOGY: OnceLock<Topology> = OnceLock::new();
        TOPOLOGY.get_or_init(|| {
            let max = __cpuid_count(0, 0).eax;
            let leaf_b = __cpuid_count(0xb, 0);
            if max >= 0xb && leaf_b.ebx & 0xffff != 0 {
                // EAX[4:0] at the SMT level: the shift to the next level's ID.
                return Topology { x2apic: true, smt_shift: leaf_b.eax & 0x1f };
            }
            // AMD without leaf 0BH: CPUID 8000_001EH EBX[15:8] is threads per
            // core minus one (APM Vol. 3), where 8000_0001H ECX[22] says the
            // topology extensions exist.
            let max_ext = __cpuid_count(0x8000_0000, 0).eax;
            let extensions = max_ext >= 0x8000_001e && __cpuid_count(0x8000_0001, 0).ecx & (1 << 22) != 0;
            let threads = if extensions { ((__cpuid_count(0x8000_001e, 0).ebx >> 8) & 0xff) + 1 } else { 1 };
            Topology { x2apic: false, smt_shift: threads.next_power_of_two().trailing_zeros() }
        })
    }

    /// The APIC ID of the CPU that ran this: leaf 0BH's EDX, or leaf 01H's
    /// EBX[31:24] where leaf 0BH enumerates nothing. Stale as soon as the
    /// scheduler moves the thread.
    pub fn current_cpu() -> u32 {
        if topology().x2apic {
            __cpuid_count(0xb, 0).edx
        } else {
            __cpuid_count(1, 0).ebx >> 24
        }
    }

    /// [`current_cpu`], as the munmap pair reads it on either system.
    pub fn cpu_id() -> Result<u32, &'static str> {
        Ok(current_cpu())
    }

    /// Which register [`cpu_id`] reads on this CPU.
    pub fn cpu_id_is() -> &'static str {
        if topology().x2apic { "cpuid.0bh.edx" } else { "cpuid.01h.ebx" }
    }

    /// The core an APIC ID belongs to: two CPUs of one core are SMT siblings.
    #[cfg(target_os = "toyos")]
    pub fn core_of(apic: u32) -> u32 {
        apic >> topology().smt_shift
    }

    #[cfg(target_os = "toyos")]
    pub fn threads_per_core() -> u32 {
        1 << topology().smt_shift
    }

    /// The last-level cache an APIC ID's CPU belongs to, or `None` where
    /// CPUID describes no cache. The highest level the deterministic cache
    /// parameters list (leaf 04H on Intel, SDM Vol. 2A; 8000_001DH on AMD,
    /// APM Vol. 3, where 8000_0001H ECX[22] says the leaf exists) is shared
    /// by EAX[25:14] + 1 logical processors, whose APIC IDs differ only below
    /// that count rounded up to a power of two.
    #[cfg(target_os = "toyos")]
    pub fn llc_of_apic(apic: u32) -> Option<u32> {
        static SHIFT: OnceLock<Option<u32>> = OnceLock::new();
        let shift = SHIFT.get_or_init(|| {
            let amd = __cpuid_count(0x8000_0000, 0).eax >= 0x8000_001d
                && __cpuid_count(0x8000_0001, 0).ecx & (1 << 22) != 0;
            let leaf = if __cpuid_count(0, 0).eax >= 4 && __cpuid_count(4, 0).eax & 0x1f != 0 {
                4
            } else if amd {
                0x8000_001d
            } else {
                return None;
            };
            (0..16)
                .map(|sub| __cpuid_count(leaf, sub).eax)
                .take_while(|eax| eax & 0x1f != 0)
                .max_by_key(|eax| (eax >> 5) & 0x7)
                .map(|eax| (((eax >> 14) & 0xfff) + 1).next_power_of_two().trailing_zeros())
        });
        shift.map(|shift| apic >> shift)
    }

    /// The processor brand string, CPUID 8000_0002H to 8000_0004H.
    #[cfg(target_os = "toyos")]
    pub fn cpu_model() -> String {
        let mut bytes = Vec::with_capacity(48);
        for leaf in 0x8000_0002u32..=0x8000_0004 {
            let r = __cpuid_count(leaf, 0);
            for reg in [r.eax, r.ebx, r.ecx, r.edx] {
                bytes.extend_from_slice(&reg.to_le_bytes());
            }
        }
        String::from_utf8_lossy(&bytes).trim_matches(char::from(0)).trim().to_string()
    }
}
