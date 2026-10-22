//! CPU identity and topology as x86-64's CPUID reads them, which answers the
//! same under any OS.

use core::arch::x86_64::__cpuid_count;
use std::sync::OnceLock;

struct Topology {
    /// CPUID leaf 0BH enumerates topology (SDM Vol. 2A: a subleaf 0 whose
    /// EBX[15:0] is 0 enumerates nothing).
    x2apic: bool,
    /// Right shift from an APIC ID to its core's ID.
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

/// The core an APIC ID belongs to: two CPUs of one core are SMT siblings.
pub fn core_of(apic: u32) -> u32 {
    apic >> topology().smt_shift
}

pub fn threads_per_core() -> u32 {
    1 << topology().smt_shift
}

/// The processor brand string, CPUID 8000_0002H to 8000_0004H.
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
