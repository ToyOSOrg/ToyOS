//! Programs `IA32_PAT` with one write-combining entry for the GOP scanout;
//! SDM Vol. 3A Table 11-7 makes an MTRR range register unnecessary.

use crate::arch::cpu;

const IA32_PAT: u32 = 0x277;

/// Architectural memory-type encodings, as a PAT entry holds them.
const UC: u8 = 0x00;
const WC: u8 = 0x01;
const WT: u8 = 0x04;
const WB: u8 = 0x06;
const UC_MINUS: u8 = 0x07;

/// The entry this kernel programs to WC, and the only one it changes: entries
/// 4..=7 select on the PAT bit alone, and 4 is the first no mapping selects.
pub const WC_ENTRY: usize = 4;

/// The entry device registers select — PCD and PWT set, the PAT bit clear.
/// UC here *and* in the power-on reset table (SDM Vol. 3A §11.12.4), so a
/// mapping through it is uncacheable whether or not [`init`] has run.
pub const UC_ENTRY: usize = 3;

/// `IA32_PAT` as this kernel programs it, one byte per entry.
const ENTRIES: [u8; 8] = [WB, WT, UC_MINUS, UC, WC, WT, UC_MINUS, UC];

const fn packed(entries: [u8; 8]) -> u64 {
    let mut value = 0u64;
    let mut i = 0;
    while i < 8 {
        value |= (entries[i] as u64) << (i * 8);
        i += 1;
    }
    value
}

const PAT_VALUE: u64 = packed(ENTRIES);

const _: () = assert!(ENTRIES[WC_ENTRY] == WC);
const _: () = assert!(ENTRIES[UC_ENTRY] == UC);

/// Put [`ENTRIES`] in this CPU's `IA32_PAT`; every CPU must run it, and no
/// rendezvous is needed because entry 4 selects nothing until it is mapped,
/// which must happen only after every CPU has run this.
pub fn init() {
    let flags: u64;
    // SAFETY: the whole sequence must run as one uninterruptible block inside
    // the no-fill window `write_cr0` opens (SDM Vol. 3A §11.12.4); `write_cr0`
    // only ever gets this CPU's own live value, first with `CD`/`NW` changed
    // and then restored, and `wrmsr` targets `IA32_PAT`, architectural on
    // every CPU in long mode.
    unsafe {
        core::arch::asm!("pushfq", "pop {}", "cli", out(reg) flags);
        let cr0 = cpu::read_cr0();
        let cr4 = cpu::read_cr4();
        // CD set with NW clear is no-fill mode: writes still hit, nothing new caches.
        cpu::write_cr0((cr0 | CR0_CD) & !CR0_NW);
        cpu::wbinvd();
        flush_tlb(cr4);

        cpu::wrmsr(IA32_PAT, PAT_VALUE);

        flush_tlb(cr4);
        cpu::wbinvd();
        cpu::write_cr0(cr0);
    }

    if flags & RFLAGS_IF != 0 {
        cpu::enable_interrupts();
    }

    // Nothing downstream can tell a wrong PAT entry from a right one; verify here.
    let read_back = cpu::rdmsr(IA32_PAT);
    assert!(
        read_back == PAT_VALUE,
        "PAT: wrote {PAT_VALUE:#018x}, IA32_PAT reads {read_back:#018x}"
    );
}

const CR0_NW: u64 = 1 << 29;
const CR0_CD: u64 = 1 << 30;
const CR4_PGE: u64 = 1 << 7;
const RFLAGS_IF: u64 = 1 << 9;

/// Flush every TLB entry, including global ones (SDM Vol. 3A §4.10.4.1).
/// # Safety
/// `cr4` must be this CPU's live `CR4`; both arms restore it verbatim.
unsafe fn flush_tlb(cr4: u64) {
    if cr4 & CR4_PGE != 0 {
        cpu::write_cr4(cr4 & !CR4_PGE);
        cpu::write_cr4(cr4);
    } else {
        cpu::write_cr3(cpu::read_cr3());
    }
}

/// This CPU's live `IA32_PAT`.
pub fn msr() -> u64 {
    cpu::rdmsr(IA32_PAT)
}

/// The type in PAT entry `index` on this CPU; its own decode, not
/// `MemoryType`, because a PAT entry may hold UC-, which no MTRR encodes.
pub fn entry_name(index: usize) -> &'static str {
    match (msr() >> (index * 8)) as u8 {
        UC => "UC",
        WC => "WC",
        WT => "WT",
        WB => "WB",
        UC_MINUS => "UC-",
        _ => "reserved",
    }
}
