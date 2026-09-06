//! The chipset's TCO watchdog, armed here so the handoff is inside the bound.
//!
//! The kernel arms and feeds the same block, on the same [`toyos_tco::PARAM`],
//! at the same [`toyos_tco::TIMER`] — but only once it is up, and this covers
//! the span before that.
//!
//! `TCO2_STS` is neither read nor written: whether the last boot ended in a TCO
//! reset is latched there and the kernel is the one place that reports it.
//!
//! Every machine this cannot arm is refused by name on the console and boots
//! anyway.

use core::mem::{align_of, size_of};
use core::ptr::read_volatile;

use toyos_acpi::Phys;
use toyos_tco::{Chipset, TCO1_CNT, TCO1_CNT_RUN, TCO_RLD, TCO_TMR, TCO_TMR_HLT};
use uefi::prelude::*;
use uefi::table::boot::{MemoryDescriptor, PAGE_SIZE};

/// x86-64's 52-bit physical-address ceiling, as `kernel/src/drivers/acpi.rs`
/// bounds the same reads.
const MAX_PHYS: u64 = 1 << 52;

/// What of the ECAM window this reads: bus 0's thirty-two devices, eight
/// functions each, one 4 KiB configuration space apiece.
const BUS_ZERO_BYTES: u64 = 32 * 8 * 4096;

/// Boot services identity-map physical memory, so a physical address is the
/// address this loader reads.
#[derive(Clone, Copy)]
struct Identity;

impl Phys for Identity {
    fn readable(self, phys: u64, len: usize) -> bool {
        phys != 0 && phys.checked_add(len as u64).is_some_and(|end| end <= MAX_PHYS)
    }

    fn byte(self, phys: u64) -> u8 {
        // SAFETY: `readable` bounded `phys` below `MAX_PHYS`, and every caller
        // in `toyos-acpi` asks it before asking this.
        unsafe { read_volatile(phys as *const u8) }
    }
}

/// Arm the watchdog when `cmdline` names it, and say on the console what was
/// armed or why nothing was.
pub fn arm(system_table: &SystemTable<Boot>, rsdp_addr: u64, cmdline: &str) {
    if !toyos_abi::boot::actuators(cmdline).any(|token| token == toyos_tco::PARAM) {
        return;
    }
    let ecam = match toyos_acpi::ecam_base(Identity, rsdp_addr) {
        Ok((_, base)) => base,
        Err(e) => return refused(format_args!("this machine's tables name no ECAM ({e:?})")),
    };
    // The MCFG's word for where configuration space is, checked against
    // firmware's own map before anything dereferences it.
    if !described(system_table, ecam, BUS_ZERO_BYTES) {
        return refused(format_args!(
            "firmware's memory map describes no {BUS_ZERO_BYTES:#x} bytes at the MCFG's {ecam:#x}"
        ));
    }
    let Some((row, base_reg, enable_reg)) = chipset(ecam) else {
        return refused(format_args!("no function on bus 0 carries a TCO block this loader knows"));
    };
    let port = match row.port(base_reg, enable_reg) {
        Ok(port) => port,
        Err(why) => {
            return refused(format_args!(
                "{:04x}:{:04x} names no TCO port ({why:?})",
                row.vendor, row.device
            ))
        }
    };

    // SAFETY: `port` is `toyos_tco`'s answer for the row this machine's own PCI
    // ids matched, and every offset written is inside that row's block.
    unsafe {
        outw(port + TCO_TMR, toyos_tco::TIMER);
        outw(port + TCO1_CNT, TCO1_CNT_RUN);
        // Reloading is also what returns the expiry count to zero.
        outw(port + TCO_RLD, 1);
    }
    // Read back: firmware may have set `TCO_LOCK`, which makes `TCO_TMR_HLT`
    // unclearable.
    let cnt = inw(port + TCO1_CNT);
    if cnt & TCO_TMR_HLT != 0 {
        return refused(format_args!("{port:#x} kept the timer halted (TCO1_CNT={cnt:#06x})"));
    }
    println!(
        "watchdog: {:04x}:{:04x} TCO at {port:#x} TCO_TMR={} armed for {}ms, and the kernel takes \
         it over",
        row.vendor,
        row.device,
        toyos_tco::TIMER,
        toyos_tco::bound_of(toyos_tco::TIMER)
    );
}

/// The first function on bus 0 a row names, with the two config words that row
/// reads. Bus 0, because the LPC bridge and the SMBus function both live there
/// and neither is behind a bridge on any machine this tree targets.
fn chipset(ecam: u64) -> Option<(&'static Chipset, u32, u32)> {
    for device in 0..32u8 {
        for function in 0..8u8 {
            let id = config_u32(ecam, device, function, 0);
            let Some(row) = toyos_tco::chipset(id as u16, (id >> 16) as u16) else { continue };
            let base = config_u32(ecam, device, function, row.base_reg);
            // One read where a chipset keeps both in one register, which q35 does.
            let enable = if row.enable.reg == row.base_reg {
                base
            } else {
                config_u32(ecam, device, function, row.enable.reg)
            };
            return Some((row, base, enable));
        }
    }
    None
}

/// Whether firmware's own map describes `[at, at + len)` inside one region.
fn described(system_table: &SystemTable<Boot>, at: u64, len: u64) -> bool {
    let bs = system_table.boot_services();
    let size = bs.memory_map_size();
    // Eight descriptors of slack: the map can grow between the two calls, and
    // this one allocates in between.
    let bytes = size.map_size + 8 * size.entry_size;
    let mut words: alloc::vec::Vec<u64> = alloc::vec![0; bytes.div_ceil(size_of::<u64>())];
    const _: () = assert!(align_of::<MemoryDescriptor>() <= align_of::<u64>());
    // SAFETY: `words` is a live allocation of exactly this many bytes, aligned
    // for `u64` and so for `MemoryDescriptor`, and nothing else names it while
    // the slice is alive.
    let buffer = unsafe {
        core::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), words.len() * 8)
    };
    let Ok(map) = bs.memory_map(buffer) else { return false };
    let Some(end) = at.checked_add(len) else { return false };
    // Every field here is firmware's, so a region whose own end overflows
    // describes nothing rather than wrapping into one that covers `at`.
    map.entries().any(|region| {
        let Some(bytes) = region.page_count.checked_mul(PAGE_SIZE as u64) else { return false };
        let Some(region_end) = region.phys_start.checked_add(bytes) else { return false };
        region.phys_start <= at && end <= region_end
    })
}

/// One configuration dword of a function on bus 0, through the ECAM window.
fn config_u32(ecam: u64, device: u8, function: u8, offset: u16) -> u32 {
    let at = ecam
        + (u64::from(device & 0x1f) << 15)
        + (u64::from(function & 7) << 12)
        + u64::from(offset & 0xffc);
    // SAFETY: the three fields above put `at` inside `[ecam, ecam +
    // BUS_ZERO_BYTES)`, which `arm` refused to enter unless firmware's own
    // memory map describes it as one region.
    unsafe { read_volatile(at as *const u32) }
}

/// # Safety
/// No fault in Ring 0; the caller owns which device answers at `port` and what
/// the word commands it to do. `kernel/src/arch/cpu.rs` states the same
/// contract for the same instruction.
unsafe fn outw(port: u16, value: u16) {
    core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
}

/// One word from an I/O port; safe because a read has no value a caller can get
/// wrong, as `kernel/src/arch/cpu.rs`'s `inw` is.
fn inw(port: u16) -> u16 {
    let value: u16;
    // SAFETY: one instruction into the declared output, no memory operand.
    unsafe {
        core::arch::asm!("in ax, dx", out("ax") value, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Why this machine is not watched, and that the boot goes on anyway.
fn refused(why: core::fmt::Arguments) {
    println!("watchdog: {why}. This boot is unwatched until the kernel arms its own");
}
