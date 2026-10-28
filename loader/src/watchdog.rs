//! The chipset's watchdog, Intel's TCO or AMD's FCH timer, armed here so the
//! handoff is inside the bound.
//!
//! The kernel arms and feeds the same timer, on the same [`toyos_tco::PARAM`],
//! at the same [`toyos_tco::BOUND_MS`] — but only once it is up, and this covers
//! the span before that. The FCH's tick is selected here and only here: its PM
//! block shares a page with registers AML uses.
//!
//! `TCO2_STS` is read and never cleared here: whether the last boot ended in
//! a TCO reset is latched there and the kernel is the one place that clears
//! it, but a boot whose kernel never reaches its own arm still owes the value,
//! and this file is the only channel such a boot has. The FCH's two records of
//! it, `S5ResetStat` and `WatchdogFired`, are this file's alone: [`survey`]
//! reads and clears them on every pass, armed or not.
//!
//! Every machine this cannot arm is refused by name on the console and boots
//! anyway.

use core::ptr::{read_volatile, write_volatile};

use alloc::string::String;
use toyos_acpi::Phys;
use toyos_tco::fch;
use toyos_tco::{
    TCO1_CNT, TCO1_CNT_LOCK, TCO1_CNT_NO_REBOOT, TCO1_CNT_RUN, TCO1_STS, TCO2_STS,
    TCO1_STS_TIMEOUT, TCO_RLD, TCO_TMR,
};
use crate::efi::{SystemTable, PAGE_SIZE};

/// x86-64's 52-bit physical-address ceiling.
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

/// The function on bus 0 whose identity names a watchdog this loader drives.
pub enum Chipset {
    /// With the two config words its row reads.
    Tco { row: &'static toyos_tco::Chipset, base: u32, enable: u32 },
    Fch { revision: u8 },
}

/// The machine's chipset watchdog, `Ok(None)` where no function names one,
/// or why bus 0 could not be read.
pub type Located = Result<Option<Chipset>, String>;

pub fn locate(system_table: &SystemTable, rsdp_addr: u64) -> Located {
    let allocations = toyos_acpi::ecam_allocations(Identity, rsdp_addr)
        .map_err(|e| alloc::format!("this machine's tables name no ECAM ({e:?})"))?;
    // Bus 0 is where the chipset is.
    let ecam = allocations
        .flatten()
        .find(|window| window.holds(0))
        .map(|window| window.base())
        .ok_or_else(|| String::from("no ECAM window of the MCFG's decodes bus 0"))?;
    // The MCFG's word for where configuration space is, checked against
    // firmware's own map before anything dereferences it.
    if !described(system_table, ecam, BUS_ZERO_BYTES) {
        return Err(alloc::format!(
            "firmware's memory map describes no {BUS_ZERO_BYTES:#x} bytes at the MCFG's {ecam:#x}"
        ));
    }
    Ok(chipset(ecam))
}

/// On an FCH: say what the last reset was, clear the FCH's record of it, and
/// stop a count this boot did not start. Every pass, armed or not.
pub fn survey(located: &Located) {
    let Ok(Some(Chipset::Fch { revision })) = located else { return };
    let survey = match fch::survey(&mut Fch) {
        Ok(survey) => survey,
        Err(why) => {
            return println!(
                "watchdog: FCH rev {revision:#04x}: ACPI-MMIO at {:#x} is not surveyed ({why:?}), so \
                 the last reset's cause is unread",
                fch::ACPI_MMIO
            )
        }
    };
    match survey.reset_stat {
        fch::ResetStat::Read { stat, after } => println!(
            "watchdog: FCH S5ResetStat={stat:#010x} on this pass's entry, written back to clear it, \
             and reads {after:#010x} after"
        ),
        fch::ResetStat::DebugSelected(select) => {
            println!("watchdog: FCH PMx0C4={select:#04x} selects S5ResetStat's debug view, so it is unread")
        }
        fch::ResetStat::Absent => println!("watchdog: FCH S5ResetStat reads all ones"),
    }
    match survey.found {
        Some(found) => println!(
            "watchdog: FCH WatchdogControl={:#010x} WatchdogCount={} on this pass's entry{}{}",
            found.control.0,
            found.count,
            if survey.stopped { ", running, so this pass stops the count the last boot left" } else { "" },
            if found.control.fired() { ", and WatchdogFired is cleared" } else { "" }
        ),
        None => println!(
            "watchdog: FCH PMx000={:#010x} leaves the watchdog undecoded, so its control is unread",
            survey.decode_en
        ),
    }
    println!(
        "watchdog: watchdog_issue_reset={} fired={}, so {}",
        u8::from(survey.issued_reset()),
        match survey.fired() {
            Some(fired) => if fired { "1" } else { "0" },
            None => "unread",
        },
        if survey.watchdog_reset() { fch::RESET_SAID } else { "no FCH watchdog reset is recorded" }
    );
}

/// Arm the watchdog when `cmdline` names it, and say on the console what was
/// armed or why nothing was.
pub fn arm(located: &Located, cmdline: &str) {
    if !toyos_abi::boot::tokens(cmdline).any(|token| token == toyos_tco::PARAM) {
        return;
    }
    match located {
        Err(why) => refused(format_args!("{why}")),
        Ok(None) => refused(format_args!("no function on bus 0 carries a chipset watchdog this loader knows")),
        Ok(Some(Chipset::Tco { row, base, enable })) => arm_tco(row, *base, *enable),
        Ok(Some(Chipset::Fch { revision })) => arm_fch(*revision),
    }
}

fn arm_tco(row: &toyos_tco::Chipset, base_reg: u32, enable_reg: u32) {
    if !crate::arch::pio::EXISTS {
        return refused(format_args!("this architecture has no I/O port space, where a TCO block answers"));
    }
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
        crate::arch::pio::outw(port + TCO_TMR, toyos_tco::TIMER);
        crate::arch::pio::outw(port + TCO1_CNT, TCO1_CNT_RUN);
        // Reloading is also what returns the expiry count to zero.
        crate::arch::pio::outw(port + TCO_RLD, 1);
    }
    // `TCO1_CNT` is judged whole apart from `TCO1_CNT_LOCK`, which the datasheet
    // says no write clears: one bit of it is not the register.
    let cnt = crate::arch::pio::inw(port + TCO1_CNT);
    if !toyos_tco::cnt_took_the_write(cnt) {
        return refused(format_args!(
            "{port:#x} did not take TCO1_CNT={TCO1_CNT_RUN:#06x}, it reads {cnt:#06x}"
        ));
    }
    println!(
        "watchdog: {:04x}:{:04x} TCO at {port:#x} TCO_TMR={} armed for {}ms, and the kernel takes \
         it over",
        row.vendor,
        row.device,
        toyos_tco::TIMER,
        toyos_tco::bound_of(toyos_tco::TIMER)
    );
    report(port, cnt);
}

fn arm_fch(revision: u8) {
    let armed = match fch::arm(&mut Fch) {
        Ok(armed) => armed,
        Err(why) => return refused(format_args!("the FCH watchdog is not armed ({why:?})")),
    };
    println!(
        "watchdog: {:04x}:{:04x} rev {revision:#04x} FCH watchdog at {:#x} WatchdogCount={} at a \
         {} ms tick armed for {}ms, and the kernel takes it over",
        fch::VENDOR,
        fch::SMBUS,
        fch::WDT,
        fch::TIMER,
        fch::TICK_MS,
        fch::bound_of(fch::TIMER)
    );
    let mut acpi = Fch;
    let reset_ctl1 = fch::AcpiMmio::read_u8(&mut acpi, fch::PM_RESET_CTL1);
    let misc = fch::AcpiMmio::read_u32(&mut acpi, fch::PM_MISC);
    let gating = fch::AcpiMmio::read_u32(&mut acpi, fch::MISC_CLK_GATING);
    let control = armed.control;
    println!(
        "watchdog: read back PMx000={:#010x} WatchdogControl={:#010x} WatchdogCount={} \
         PMx0BE={reset_ctl1:#04x} PMx0C8={misc:#010x} MISCx0FC={gating:#010x}{}",
        armed.decode_en,
        control.0,
        armed.count,
        if armed.cleared_fired { ", with a stale WatchdogFired cleared" } else { "" }
    );
    // What decides whether an expiry resets this machine, and how: named
    // rather than left for a reader to mask out. A warm reset is CPURST#
    // alone; `RstToCpuPwrGdEn` makes every FCH reset a cold one (55570 §9.1.11).
    let resets = !control.powers_off() && misc & fch::USE_CPU_RST != 0;
    println!(
        "watchdog: power_off={} use_cpu_rst={} cold={} clock_gating_dis={} action_en={}, so an \
         expiry {} reset this machine",
        u8::from(control.powers_off()),
        u8::from(misc & fch::USE_CPU_RST != 0),
        u8::from(reset_ctl1 & fch::RST_TO_CPU_PWR_GD_EN != 0),
        // All ones is also what an undecoded register reads.
        match gating {
            u32::MAX => "unread",
            gating if gating & fch::WDT_BLK_CLK_GATING_DIS != 0 => "1",
            _ => "0",
        },
        u8::from(control.0 & fch::Control::ACTION_EN != 0),
        if resets { "can" } else { "cannot" },
    );
}

/// The FCH's two windows at the addresses AMD fixes for them, which boot
/// services identity-map.
struct Fch;

impl Fch {
    fn acpi(offset: u16, len: u64) -> u64 {
        assert!(u64::from(offset) + len <= fch::ACPI_MMIO_BYTES, "ACPI-MMIO offset {offset:#x}");
        fch::ACPI_MMIO + u64::from(offset)
    }
}

// SAFETY, for every access below: the address is inside the window AMD's PPR
// fixes for an FCH whose PCI identity `chipset` matched, and each is one
// volatile access of the register's own width.
impl fch::AcpiMmio for Fch {
    fn read_u8(&mut self, offset: u16) -> u8 {
        // SAFETY: as above.
        unsafe { read_volatile(Self::acpi(offset, 1) as *const u8) }
    }

    fn write_u8(&mut self, offset: u16, value: u8) {
        // SAFETY: as above.
        unsafe { write_volatile(Self::acpi(offset, 1) as *mut u8, value) }
    }

    fn read_u32(&mut self, offset: u16) -> u32 {
        // SAFETY: as above.
        unsafe { read_volatile(Self::acpi(offset, 4) as *const u32) }
    }

    fn write_u32(&mut self, offset: u16, value: u32) {
        // SAFETY: as above.
        unsafe { write_volatile(Self::acpi(offset, 4) as *mut u32, value) }
    }
}

impl fch::Window for Fch {
    fn read(&mut self, register: fch::Register) -> u32 {
        // SAFETY: as above.
        unsafe { read_volatile((fch::WDT + register as u64) as *const u32) }
    }

    fn write(&mut self, register: fch::Register, value: u32) {
        // SAFETY: as above.
        unsafe { write_volatile((fch::WDT + register as u64) as *mut u32, value) }
    }
}

/// What the block holds once it is armed, as whole words.
fn report(port: u16, cnt: u16) {
    let rld = crate::arch::pio::inw(port + TCO_RLD);
    let tmr = crate::arch::pio::inw(port + TCO_TMR);
    let sts1 = crate::arch::pio::inw(port + TCO1_STS);
    let sts2 = crate::arch::pio::inw(port + TCO2_STS);
    println!(
        "watchdog: read back TCO_RLD={rld:#06x} TCO_TMR={tmr:#06x} TCO1_CNT={cnt:#06x} \
         TCO1_STS={sts1:#06x} TCO2_STS={sts2:#06x}"
    );
    // The three bits that decide whether an expiry reaches this machine at all,
    // named rather than left for a reader to mask out. `TIMEOUT` is the first
    // expiry's own latch, so a machine that is still up with it set expired once
    // and did not reboot.
    println!(
        "watchdog: no_reboot={} tco_lock={} timeout={}, so a second expiry {} reset this machine",
        u8::from(cnt & TCO1_CNT_NO_REBOOT != 0),
        u8::from(cnt & TCO1_CNT_LOCK != 0),
        u8::from(sts1 & TCO1_STS_TIMEOUT != 0),
        if cnt & TCO1_CNT_NO_REBOOT != 0 { "cannot" } else { "can" },
    );
}

/// The first function on bus 0 that names a watchdog. Bus 0, because the
/// LPC bridge and the SMBus function both live there and neither is behind a
/// bridge on any machine this tree targets.
fn chipset(ecam: u64) -> Option<Chipset> {
    for device in 0..32u8 {
        for function in 0..8u8 {
            let id = config_u32(ecam, device, function, 0);
            let (vendor, device_id) = (id as u16, (id >> 16) as u16);
            if let Some(row) = toyos_tco::chipset(vendor, device_id) {
                let base = config_u32(ecam, device, function, row.base_reg);
                // One read where a chipset keeps both in one register, which q35 does.
                let enable = if row.enable.reg == row.base_reg {
                    base
                } else {
                    config_u32(ecam, device, function, row.enable.reg)
                };
                return Some(Chipset::Tco { row, base, enable });
            }
            let revision = config_u32(ecam, device, function, 0x08) as u8;
            if fch::carries(vendor, device_id, revision) {
                return Some(Chipset::Fch { revision });
            }
        }
    }
    None
}

/// Whether firmware's own map describes `[at, at + len)` inside one region.
fn described(system_table: &SystemTable, at: u64, len: u64) -> bool {
    let bs = system_table.boot_services();
    let (map_size, entry_size) = bs.memory_map_size();
    // Eight descriptors of slack: the map can grow between the two calls, and
    // this one allocates in between.
    let bytes = map_size + 8 * entry_size;
    let mut words: alloc::vec::Vec<u64> = alloc::vec![0; bytes.div_ceil(size_of::<u64>())];
    let Ok(map) = bs.memory_map(&mut words) else { return false };
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

/// Why this machine is not watched, and that the boot goes on anyway.
fn refused(why: core::fmt::Arguments) {
    println!("watchdog: {why}. This boot is unwatched until the kernel arms its own");
}
