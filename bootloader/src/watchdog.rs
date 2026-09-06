//! The chipset's TCO watchdog, armed here so the handoff is inside the bound.
//!
//! The kernel arms and feeds the same timer, but only once it is up: a machine
//! that stops between this jump and `drivers::watchdog::init` has nothing
//! watching it and stays down until somebody walks to it. This arms the same
//! block, on the same [`toyos_tco::PARAM`], at the same [`toyos_tco::TIMER`],
//! and leaves the kernel to take it over.
//!
//! `TCO2_STS` is not touched. Whether the last boot ended in a TCO reset is
//! latched there and the kernel reports it; a read-modify-write here would take
//! that evidence away from the one place that reads it.
//!
//! A machine no row in [`toyos_tco::CHIPSETS`] names is refused by name on the
//! console and the boot continues.

use core::ptr::read_volatile;

use toyos_acpi::Phys;
use uefi_services::println;
use toyos_tco::{Chipset, TCO1_CNT, TCO1_CNT_RUN, TCO_RLD, TCO_TMR, TCO_TMR_HLT};

/// x86-64's 52-bit physical-address ceiling, as `kernel/src/drivers/acpi.rs`
/// bounds the same reads.
const MAX_PHYS: u64 = 1 << 52;

/// Firmware's tables, read where firmware left them: boot services identity-map
/// physical memory, so a physical address is the address this loader reads.
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
pub fn arm(rsdp_addr: u64, cmdline: &[u8]) {
    let Ok(cmdline) = core::str::from_utf8(cmdline) else { return };
    if !toyos_abi::boot::actuators(cmdline).any(|token| token == toyos_tco::PARAM) {
        return;
    }
    let ecam = match toyos_acpi::ecam_base(Identity, rsdp_addr) {
        Ok((_, base)) => base,
        Err(e) => return refused(format_args!("this machine's tables name no ECAM ({e:?})")),
    };
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
    // unclearable. SAFETY: as the writes above.
    let cnt = unsafe { inw(port + TCO1_CNT) };
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

/// One configuration dword of a function on bus 0, through the ECAM window.
///
/// `device`, `function` and `offset` are this file's own and inside their
/// fields; an absent function reads all ones, which every caller refuses.
fn config_u32(ecam: u64, device: u8, function: u8, offset: u16) -> u32 {
    let at = ecam
        + (u64::from(device) << 15)
        + (u64::from(function) << 12)
        + u64::from(offset & !3);
    // SAFETY: the ECAM window is 256 MiB from `base` and firmware identity-maps
    // it under boot services; `device`, `function` and `offset` are bounded by
    // their callers to bits 15..20, 12..15 and 0..12 of one function's 4 KiB.
    unsafe { read_volatile(at as *const u32) }
}

/// # Safety
/// The caller must name a port it is entitled to write.
unsafe fn outw(port: u16, value: u16) {
    core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
}

/// # Safety
/// The caller must name a port it is entitled to read.
unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    core::arch::asm!("in ax, dx", out("ax") value, in("dx") port, options(nomem, nostack, preserves_flags));
    value
}

/// Why this machine is not watched, and that the boot goes on anyway.
fn refused(why: core::fmt::Arguments) {
    println!("watchdog: {why}. This boot is unwatched until the kernel arms its own");
}
