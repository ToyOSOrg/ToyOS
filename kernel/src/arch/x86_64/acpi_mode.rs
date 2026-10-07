//! The machine's ACPI mode, and the row a process serves its SCI through.
//!
//! **This kernel puts the machine in ACPI mode only for a holder of the
//! `acpi` claim, and back in the mode its firmware handed over when the claim
//! goes.** The mint writes `ACPI_ENABLE` to `SMI_CMD` (ACPI 6.5 Table 5.9)
//! where `SCI_EN` reads clear, and waits for the firmware to set it; the
//! release writes `ACPI_DISABLE` where the mint wrote the enable and reads
//! `SCI_EN` until it is clear, with the row still held, so a dead server
//! leaves the buttons to the firmware again before anyone else can claim
//! them, and so does a mint that wrote the enable and then failed. A firmware
//! that does not clear it is said by name, and the next release writes the
//! disable again. **Neither is written once the stop has
//! begun**: the power-off waits out a write in flight and then owns the
//! hardware, and an SMI it did not make is one its S5 entry was never
//! measured against.
//!
//! **A machine stays in legacy mode where its firmware serves something no
//! holder could**: an embedded controller the ECDT does not name, or a power
//! button that is a control method device, which only AML serves. Both are
//! refused at the mint, by name; a machine the firmware handed over in ACPI
//! mode is claimed whatever it has, since nothing is written.
//!
//! **The ECDT is a stopgap**: the embedded controller is read from it until the
//! interpreter reads the controller's own device from the DSDT, and then this
//! path and `toyos_acpi::ecdt` go
//! (`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`).
//!
//! The row is the FADT's PM1a event and GPE0 blocks and the ECDT's two
//! ports, filled once at boot; the SCI is its one line, level.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use toyos_abi::acpi::{AcpiInfo, Block, FIXED_POWER_BUTTON};
use toyos_acpi::{Ec, FixedHardware, LegacyMode, PowerButton};
use toyos_userbound::Ports;

use super::cpu;
use super::pio::{self, Declared, TakenBack};
use super::power::SCI_EN;
use crate::device::ClaimError;
use crate::isa::{self, Function};
use crate::log;
use crate::sync::{Lock, LockGuard};
use crate::time::{Deadline, Duration};

/// `isa`'s row for the fixed hardware.
pub const ROW: usize = 1;
/// The row's runs, at the places [`init`] fills them in.
const PM1_EVENTS: usize = 0;
const GPE0: usize = 1;

/// How long the firmware has to set `SCI_EN` after the enable: one that has
/// not answered in this will not.
const HANDOVER: Duration = Duration::from_secs(3);
/// How often the wait reads `SCI_EN`, parked in between.
const POLL: Duration = Duration::from_millis(1);
/// How long the firmware has to clear `SCI_EN` after the disable, spun. ACPI
/// 6.5 §4.8.2.5 has OSPM poll the bit until it reads reset and names no bound,
/// and no FADT field carries one: this is this kernel's, and no measurement.
const HANDBACK: Duration = Duration::from_millis(100);

struct Hardware {
    fixed: FixedHardware,
    control: Declared,
    /// `SMI_CMD`, declared, and what is written to it.
    legacy: Option<(Declared, LegacyMode)>,
    /// Or why it is none a holder can be handed.
    ec: Result<Ec, String>,
}

/// Written once, by [`init`].
static HARDWARE: AtomicPtr<Hardware> = AtomicPtr::new(core::ptr::null_mut());

/// This kernel wrote `ACPI_ENABLE` and has not read `SCI_EN` clear since, so
/// the release writes `ACPI_DISABLE`.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Held across every write to `SMI_CMD`.
static SMI_CMD_WRITE: Lock<()> = Lock::new(());

/// The right to write `SMI_CMD`, held across the write; none once the stop has
/// begun.
fn smi_cmd_write() -> Option<LockGuard<'static, ()>> {
    let writing = SMI_CMD_WRITE.lock();
    (!crate::quiesce::begun()).then_some(writing)
}

/// Wait out a write to `SMI_CMD` in flight: the stop has begun, so none follows.
pub fn settle(_taken: &TakenBack) {
    drop(SMI_CMD_WRITE.lock());
}

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
    let Some(control) = super::power::pm1a_control() else {
        return log!("acpi: no ACPI row — no PM1a control block declared");
    };
    let declared = |legacy: LegacyMode| pio::declare("SMI_CMD", Ports::one(legacy.smi_cmd)).map(|port| (port, legacy));
    let legacy = match fixed.legacy.map(declared).transpose() {
        Ok(legacy) => legacy,
        Err(why) => return log!("acpi: no ACPI row — SMI_CMD not declared: {why:?}"),
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
    let was = HARDWARE.swap(Box::into_raw(Box::new(Hardware { fixed, control, legacy, ec })), Ordering::Release);
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
        if ENABLED.load(Ordering::Relaxed) {
            log!("acpi: this machine is still in ACPI mode after an ACPI_DISABLE its firmware did not act on, so nothing is written");
        } else {
            log!("acpi: the firmware handed this machine over in ACPI mode, so nothing is written");
        }
        return Ok(());
    }
    let refuse = |why: &str| {
        log!("acpi: this machine stays in legacy mode — {why}");
        Err(ClaimError::Unusable)
    };
    let Some((smi_cmd, legacy)) = hardware.legacy else {
        return refuse("the FADT names no SMI_CMD, ACPI_ENABLE and ACPI_DISABLE to leave it and come back with");
    };
    let enable = legacy.acpi_enable.get();
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
        let Some(_writing) = smi_cmd_write() else {
            return refuse("the machine is stopping");
        };
        let _closed = crate::arch::IrqGuard::close();
        let before = super::counters::read().smi;
        // SAFETY: `SMI_CMD`, declared, and the value the FADT names for it.
        unsafe { cpu::outb(smi_cmd.port(0), enable) };
        ENABLED.store(true, Ordering::Relaxed);
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
    log!(
        "acpi: ACPI mode: ACPI_ENABLE {enable:#04x} written to SMI_CMD {:#x}, SCI_EN set {} after; cpu{me}'s SMI count {} before the write and {} after",
        smi_cmd.ports().first(),
        crate::clock::now() - written,
        before.map_or_else(|| "unread".into(), |n| format!("{n}")),
        after.map_or_else(|| "unread".into(), |n| format!("{n}")),
    );
    Ok(())
}

/// The claim's last handle went: the machine back in legacy mode where the
/// mint took it out of it, and then the row released, so no claimant finds
/// `SCI_EN` set by a holder whose disable is still to come.
pub fn release(row: usize) {
    if ENABLED.load(Ordering::Relaxed) {
        leave(hardware().expect("a claimed row has its hardware"));
    }
    isa::release(row);
}

/// `ACPI_DISABLE`, written where the mint wrote `ACPI_ENABLE`, and `SCI_EN`
/// read until it is clear; nothing once the stop has begun. Spun, never
/// parked: the task a claim's last handle goes with may be dying.
fn leave(hardware: &Hardware) {
    let (smi_cmd, legacy) = hardware.legacy.expect("ACPI_ENABLE was written to it");
    let disable = legacy.acpi_disable.get();
    let Some(writing) = smi_cmd_write() else {
        return log!("acpi: ACPI_DISABLE not written: the machine is stopping, and its power-off owns ACPI mode");
    };
    // SAFETY: `SMI_CMD`, declared, and the value the FADT names for it.
    unsafe { cpu::outb(smi_cmd.port(0), disable) };
    drop(writing);
    let written = crate::clock::now();
    let by = Deadline::at(written + HANDBACK);
    let control = loop {
        let control = cpu::inw(hardware.control.port(0));
        if control & SCI_EN == 0 {
            break control;
        }
        if by.reached(crate::clock::now()) {
            return log!(
                "acpi: still in ACPI mode: ACPI_DISABLE {disable:#04x} written to SMI_CMD, and PM1a_CNT reads {control:#06x} \
                 {HANDBACK} after, SCI_EN still set: nothing serves this machine's buttons until a holder claims them"
            );
        }
        core::hint::spin_loop();
    };
    ENABLED.store(false, Ordering::Relaxed);
    log!(
        "acpi: legacy mode again: ACPI_DISABLE {disable:#04x} written to SMI_CMD, PM1a_CNT reads {control:#06x} {} after, \
         SCI_EN clear",
        crate::clock::now() - written,
    );
}

/// PM1 status bits a write of one clears (ACPI 6.5 Table 4.13): timer, bus
/// master, global lock release, power button, sleep button, RTC, PCIe wake
/// and wake.
const PM1_STATUS: u16 = 1 << 0 | 1 << 4 | 1 << 5 | 1 << 8 | 1 << 9 | 1 << 10 | 1 << 14 | 1 << 15;

/// What the PM1 event block reads, status then enable, for a power-off that
/// did not take.
pub fn pm1_events(taken: &TakenBack) -> String {
    let Some(hardware) = hardware() else { return "no ACPI row, so no PM1 event block read".into() };
    let events = taken.run(ROW, PM1_EVENTS);
    let half = hardware.fixed.pm1a_event.len / 2;
    format!("PM1 status {:#06x} under enable {:#06x}", cpu::inw(events.port(0)), cpu::inw(events.port(half)))
}

/// Every fixed and general-purpose event disabled and its status cleared:
/// the power-off's, on a machine in ACPI mode.
pub fn quiet(taken: &TakenBack) {
    let Some(hardware) = hardware() else { return };
    let events = taken.run(ROW, PM1_EVENTS);
    let half = hardware.fixed.pm1a_event.len / 2;
    // SAFETY: the PM1a event block the FADT names, taken back from any holder.
    unsafe {
        cpu::outw(events.port(half), 0);
        cpu::outw(events.port(0), PM1_STATUS);
    }
    if hardware.fixed.gpe0.len == 0 {
        return;
    }
    let gpe = taken.run(ROW, GPE0);
    let half = hardware.fixed.gpe0.len / 2;
    for byte in 0..half {
        // SAFETY: the GPE0 block, taken back as the PM1 block is; its status bits clear on a one.
        unsafe {
            cpu::outb(gpe.port(half + byte), 0);
            cpu::outb(gpe.port(byte), 0xFF);
        }
    }
}
