//! The machine's ACPI mode, and the row a process serves its SCI through.
//!
//! **This kernel puts the machine in ACPI mode only for a holder of the
//! `acpi` claim, and back in the mode its firmware handed over when the claim
//! goes.** The mint writes `ACPI_ENABLE` to `SMI_CMD` (ACPI 6.5 Table 5.9)
//! where `SCI_EN` reads clear, and waits for the firmware to set it; the
//! release writes `ACPI_DISABLE` where the mint wrote the enable, so a dead
//! server leaves the buttons to the firmware again, and so does a mint that
//! wrote the enable and then failed.
//!
//! **A machine stays in legacy mode where its firmware serves something no
//! holder could**: an embedded controller the ECDT does not name, or a power
//! button that is a control method device, which only AML serves. Both are
//! refused at the mint, by name; a machine the firmware handed over in ACPI
//! mode is claimed whatever it has, since nothing is written.
//!
//! The row is the FADT's PM1a event and GPE0 blocks and the ECDT's two
//! ports, filled once at boot; the SCI is its one line, level.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use toyos_abi::acpi::{AcpiInfo, Block, FIXED_POWER_BUTTON};
use toyos_acpi::{Ec, FixedHardware, PowerButton};
use toyos_userbound::Ports;

use super::cpu;
use super::pio::{self, Declared};
use super::power::SCI_EN;
use crate::device::ClaimError;
use crate::isa::{self, Function};
use crate::log;
use crate::time::{Deadline, Duration};

/// `isa`'s row for the fixed hardware.
pub const ROW: usize = 1;

/// How long the firmware has to set `SCI_EN` after the enable: one that has
/// not answered in this will not. The T14 answers in 2.13 ms
/// (`issues/hardware/the-t14s-firmware-interrupts-every-cpu-every-2-2-s-under-toyos.md`).
const HANDOVER: Duration = Duration::from_secs(3);
/// How often the wait reads `SCI_EN`, parked in between.
const POLL: Duration = Duration::from_millis(1);

struct Hardware {
    fixed: FixedHardware,
    control: Declared,
    smi_cmd: Option<Declared>,
    /// Or why it is none a holder can be handed.
    ec: Result<Ec, String>,
}

/// Written once, by [`init`].
static HARDWARE: AtomicPtr<Hardware> = AtomicPtr::new(core::ptr::null_mut());

/// The mint wrote `ACPI_ENABLE`, so the release writes `ACPI_DISABLE`.
static ENABLED: AtomicBool = AtomicBool::new(false);

fn hardware() -> Option<&'static Hardware> {
    let at = HARDWARE.load(Ordering::Acquire);
    // SAFETY: `init` stored a leaked `Box` once, and nothing frees it.
    (!at.is_null()).then(|| unsafe { &*at })
}

/// Decode the fixed hardware, declare `SMI_CMD`, and fill the row; or say by
/// name why this machine has none. After the i8042's row, whose lines the
/// SCI may not share.
pub fn init(rsdp_addr: u64) {
    let fadt = match toyos_acpi::find_table(
        crate::drivers::acpi::direct_phys(),
        rsdp_addr,
        b"FACP",
        toyos_acpi::FADT_FOR_FIXED_HARDWARE,
    ) {
        Ok(fadt) => fadt,
        Err(e) => return log!("acpi: no ACPI row — the FADT is unusable: {e:?}"),
    };
    let fixed = match toyos_acpi::fixed_hardware(&fadt) {
        Ok(fixed) => fixed,
        Err(refused) => return log!("acpi: no ACPI row — the FADT's fixed hardware is none this kernel serves: {refused:?}"),
    };
    let Some(control) = super::power::pm1a_control().filter(|c| c.ports().first() == fixed.pm1a_control.port) else {
        return log!("acpi: no ACPI row — the PM1a control block {:#x} is not the one this kernel declared", fixed.pm1a_control.port);
    };
    let smi_cmd = match fixed.smi_cmd.map(|port| pio::declare("SMI_CMD", Ports::one(port))) {
        None => None,
        Some(Ok(declared)) => Some(declared),
        Some(Err(why)) => return log!("acpi: no ACPI row — SMI_CMD not declared: {why:?}"),
    };
    let ec = embedded_controller(rsdp_addr, fixed.gpe0);
    let Some(sci) = super::ioapic::sci(fixed.sci_int) else {
        return log!("acpi: no ACPI row — no I/O APIC carries the SCI");
    };
    if let Some(sharer) = isa::line_holder(sci) {
        return log!("acpi: no ACPI row — the SCI's {} is {sharer}'s line too", pio::describe(sci));
    }

    let mut runs = vec![run(fixed.pm1a_event)];
    if fixed.gpe0.len != 0 {
        runs.push(run(fixed.gpe0));
    }
    if let Ok(ec) = &ec {
        runs.extend([Ports::one(ec.command), Ports::one(ec.data)]);
    }
    log!(
        "acpi: the ACPI row: PM1a events {:#x}+{}, GPE0 {:#x}+{}, SCI {}, {}, embedded controller {}; the firmware handed over in {} mode",
        fixed.pm1a_event.port,
        fixed.pm1a_event.len,
        fixed.gpe0.port,
        fixed.gpe0.len,
        pio::describe(sci),
        match fixed.power_button {
            PowerButton::Fixed => "the fixed-hardware power button",
            PowerButton::ControlMethod => "a control-method power button",
        },
        match &ec {
            Ok(ec) => format!("at {:#x}/{:#x} on GPE {:#x}", ec.command, ec.data, ec.gpe),
            Err(why) => format!("none ({why})"),
        },
        if cpu::inw(control.port(0)) & SCI_EN != 0 { "ACPI" } else { "legacy" },
    );
    isa::fill(ROW, Function { name: "the ACPI fixed hardware", runs, irqs: vec![], wires: vec![sci] });
    let was = HARDWARE.swap(Box::into_raw(Box::new(Hardware { fixed, control, smi_cmd, ec })), Ordering::Release);
    assert!(was.is_null(), "acpi: init ran twice");
}

fn run(block: Block) -> Ports {
    Ports::new(block.port, block.len).expect("`fixed_hardware` bounded every block by the port space")
}

fn embedded_controller(rsdp_addr: u64, gpe0: Block) -> Result<Ec, String> {
    let ecdt = toyos_acpi::find_table(crate::drivers::acpi::direct_phys(), rsdp_addr, b"ECDT", toyos_acpi::ECDT_NEEDED)
        .map_err(|e| format!("the ECDT is unusable: {e:?}"))?;
    let ec = toyos_acpi::ecdt(&ecdt).map_err(|refused| format!("the ECDT names none: {refused:?}"))?;
    // §5.2.9: a GPE block's status half holds eight GPEs a byte.
    if u16::from(ec.gpe) >= gpe0.len / 2 * 8 {
        return Err(format!("the ECDT puts it on GPE {:#x}, outside GPE0's {}", ec.gpe, gpe0.len / 2 * 8));
    }
    Ok(ec)
}

/// The row, claimed, with the machine in ACPI mode; or the refusal that
/// leaves it in legacy mode, said by name.
pub fn claim() -> Result<(usize, AcpiInfo), ClaimError> {
    let hardware = hardware().ok_or(ClaimError::Absent)?;
    let row = isa::claim_row(ROW)?;
    match enter(hardware) {
        Ok(()) => Ok((row, info(hardware))),
        Err(refused) => {
            isa::release(row);
            Err(refused)
        }
    }
}

fn info(hardware: &Hardware) -> AcpiInfo {
    let (ec_command, ec_data, ec_gpe) = match &hardware.ec {
        Ok(ec) => (Block { port: ec.command, len: 1 }, Block { port: ec.data, len: 1 }, u16::from(ec.gpe)),
        Err(_) => (Block::NONE, Block::NONE, 0),
    };
    AcpiInfo {
        pm1_event: hardware.fixed.pm1a_event,
        gpe0: hardware.fixed.gpe0,
        ec_command,
        ec_data,
        ec_gpe,
        flags: if hardware.fixed.power_button == PowerButton::Fixed { FIXED_POWER_BUTTON } else { 0 },
    }
}

fn sci_enabled(hardware: &Hardware) -> bool {
    cpu::inw(hardware.control.port(0)) & SCI_EN != 0
}

fn enter(hardware: &Hardware) -> Result<(), ClaimError> {
    if sci_enabled(hardware) {
        log!("acpi: the firmware handed this machine over in ACPI mode, so nothing is written");
        return Ok(());
    }
    let refuse = |why: &str| {
        log!("acpi: this machine stays in legacy mode — {why}");
        Err(ClaimError::Unusable)
    };
    let (Some(smi_cmd), enable @ 1..) = (hardware.smi_cmd, hardware.fixed.acpi_enable) else {
        return refuse("the FADT names no SMI_CMD and ACPI_ENABLE to leave it with");
    };
    if let Err(why) = &hardware.ec {
        return refuse(&format!(
            "its firmware serves an embedded controller no holder could ({why})"
        ));
    }
    if hardware.fixed.power_button == PowerButton::ControlMethod {
        return refuse("its power button is a control method device, which only AML serves");
    }

    // The write raises a firmware interrupt where `APMC_EN` is set, counted on
    // the CPU that makes it, so both reads are that CPU's.
    let (me, before, after) = {
        let _closed = crate::arch::IrqGuard::close();
        let before = super::counters::read().smi;
        // SAFETY: `SMI_CMD`, declared, and the value the FADT names for it.
        unsafe { cpu::outb(smi_cmd.port(0), enable) };
        (super::percpu::cpu_id(), before, super::counters::read().smi)
    };
    let written = crate::clock::now();
    let by = Deadline::at(written + HANDOVER);
    let parkable = crate::scheduler::Parkable::at_entry();
    let handle = crate::sched::driver::current_handle().expect("acpi: a claim minted by no task");
    while !sci_enabled(hardware) {
        if by.reached(crate::clock::now()) {
            leave(hardware);
            return refuse(&format!("SCI_EN still reads clear {HANDOVER} after ACPI_ENABLE was written"));
        }
        let poll = Deadline::at(crate::clock::now() + POLL);
        let parked = crate::watch::wait_until(&parkable, handle.watch(), 0, kernel::sched::task::WaitClass::Other, poll, || false);
        if parked.is_err() {
            leave(hardware);
            return refuse("its claimant was killed waiting for SCI_EN");
        }
    }
    ENABLED.store(true, Ordering::Relaxed);
    log!(
        "acpi: ACPI mode: ACPI_ENABLE {enable:#04x} written to SMI_CMD {:#x}, SCI_EN set {} after; cpu{me}'s SMI count {} before the write and {} after",
        smi_cmd.ports().first(),
        crate::clock::now() - written,
        before.map_or_else(|| "unread".into(), |n| format!("{n}")),
        after.map_or_else(|| "unread".into(), |n| format!("{n}")),
    );
    Ok(())
}

/// The claim's last handle went: the row released, and the machine back in
/// legacy mode where the mint took it out of it.
pub fn release(row: usize) {
    isa::release(row);
    if ENABLED.swap(false, Ordering::Relaxed) {
        leave(hardware().expect("a claimed row has its hardware"));
    }
}

/// `ACPI_DISABLE`, written where the mint wrote `ACPI_ENABLE`, and what `SCI_EN`
/// reads straight after.
fn leave(hardware: &Hardware) {
    let smi_cmd = hardware.smi_cmd.expect("ACPI_ENABLE was written to it");
    // SAFETY: `SMI_CMD`, declared, and the value the FADT names for it.
    unsafe { cpu::outb(smi_cmd.port(0), hardware.fixed.acpi_disable) };
    let control = cpu::inw(hardware.control.port(0));
    log!(
        "acpi: legacy mode again: ACPI_DISABLE {:#04x} written to SMI_CMD, PM1a_CNT reads {control:#06x}, SCI_EN {}",
        hardware.fixed.acpi_disable,
        if control & SCI_EN == 0 { "clear" } else { "still set" },
    );
}

/// PM1 status bits a write of one clears (ACPI 6.5 Table 4.13): timer, bus
/// master, global lock release, power button, sleep button, RTC, PCIe wake
/// and wake.
const PM1_STATUS: u16 = 1 << 0 | 1 << 4 | 1 << 5 | 1 << 8 | 1 << 9 | 1 << 10 | 1 << 14 | 1 << 15;

/// Every fixed and general-purpose event disabled and its status cleared:
/// the power-off's, on a machine in ACPI mode, once userland has stopped.
pub fn quiet() {
    let Some(hardware) = hardware() else { return };
    let events = pio::taken_back(run(hardware.fixed.pm1a_event));
    let half = hardware.fixed.pm1a_event.len / 2;
    // SAFETY: the PM1a event block the FADT names, taken back from a holder that no longer runs.
    unsafe {
        cpu::outw(events.port(half), 0);
        cpu::outw(events.port(0), PM1_STATUS);
    }
    if hardware.fixed.gpe0.len == 0 {
        return;
    }
    let gpe = pio::taken_back(run(hardware.fixed.gpe0));
    let half = hardware.fixed.gpe0.len / 2;
    for byte in 0..half {
        // SAFETY: the GPE0 block, taken back as the PM1 block is; its status bits clear on a one.
        unsafe {
            cpu::outb(gpe.port(half + byte), 0);
            cpu::outb(gpe.port(byte), 0xFF);
        }
    }
}
