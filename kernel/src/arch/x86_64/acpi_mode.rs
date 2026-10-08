//! The machine's ACPI mode, and the row a process serves its SCI through.
//!
//! **This kernel puts the machine in ACPI mode only for a holder of the
//! `acpi` claim, and back in the mode its firmware handed over when the claim
//! goes.** The mint writes `ACPI_ENABLE` to `SMI_CMD` (ACPI 6.5 Table 5.9),
//! through `smi_cmd::write` as every write to that port is and so on the boot
//! processor, where `SCI_EN` reads clear, and waits for the firmware to set it; the
//! release writes `ACPI_DISABLE` where the mint wrote the enable and reads
//! `SCI_EN` until it is clear, with the row still held, so a dead server
//! leaves the buttons to the firmware again before anyone else can claim
//! them, and so does a mint that wrote the enable and then failed. A firmware
//! that does not clear it is said by name, and the next release writes the
//! disable again. Neither is written once the stop has begun: `smi_cmd`
//! refuses it.
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
//!
//! **What the firmware's AML names outside the row, this kernel reads and
//! writes for the claim's holder, one access at a time** ([`access`]): memory
//! through the direct map and a port both ways, a function's configuration
//! space through ECAM to read, each only with the witness
//! `toyos_userbound::firmware` answered for it. Firmware's memory is typed by
//! firmware's own map and by its MTRRs: the direct map's leaves select the
//! PAT's write-back entry, under which the range registers decide (Intel SDM
//! Vol. 3A, Table 12-7), so a register window the firmware reserved is read as
//! the firmware typed it. A register at an address the firmware's map does
//! not list is read the same way, only where the boot processor's registers,
//! read once at boot, type it uncacheable, and only on a machine none of
//! whose CPUs holds registers that are on and not those (`mtrr::compare`):
//! the read is made on whichever CPU the call runs on.
//!
//! **A byte the holder's AML stores to `SMI_CMD` is a call into the firmware,
//! made here and by nobody else** ([`call`]): the policy answers which byte,
//! `smi_cmd::write` makes it on the boot processor and returns once the
//! firmware's handler has, and the holder's thread is the one that waits and
//! is charged. What the handler does with the byte, and with whatever the
//! holder wrote to firmware's memory before it, nothing here bounds: system
//! management mode outranks this kernel. What is bounded is who, a holder of
//! the claim; when, never once the stop has begun; where, the boot processor;
//! which byte, none the FADT gives a meaning; and how often,
//! `firmware::CALLS` in any second, because the AML that calls retries a
//! call its handler has not answered and each call stops every CPU. A call
//! past that is refused to the caller by name and written nowhere.
//!
//! **The firmware's Global Lock is taken and given back here** (ACPI 6.5
//! §5.2.10.1), by compare-and-exchange on the FACS's lock word; a release the
//! firmware asked for meanwhile is signalled by `GBL_RLS` in `PM1a_CNT`
//! (§4.8.3.2). A lock its holder left taken goes back with the claim, and
//! before the power-off, so SMM never waits on a process that is gone. A
//! machine whose FADT names no FACS has no lock, and every take is answered
//! taken; one whose FACS this kernel refuses has a lock nothing here can
//! take, and every take is refused ([`GlobalLock`]).
//!
//! **The power-off's sleep type is the holder's to supply** ([`s5`]), once
//! under a claim: `\_S5` is AML, and this kernel reads none.
//!
//! **Nothing is done for the holder once the stop has begun**, as nothing is
//! written to `SMI_CMD`: an access, a lock exchange and a sleep type taken are each made under
//! [`HOLDER`], from the decision to the last instruction, and refused there
//! once the stop has begun; the power-off takes that lock before it owns the
//! hardware ([`settle`]), which waits out the one in flight.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use core::sync::atomic::AtomicU32;

use toyos_abi::acpi::{Access, AcpiInfo, Block, Refused, Space, Width, FIXED_POWER_BUTTON};
use toyos_abi::syscall::SyscallError;
use toyos_acpi::{Ec, FixedHardware, LegacyMode, PowerButton};
use toyos_userbound::firmware::{
    self, CallRate, Ecam, FirmwareCall, Function as PciFunction, LockWordAt, Memory, MemoryAt, MemoryVerdict, PortAt, PortVerdict,
};
use toyos_userbound::Ports;

use super::pio::{self, Declared, TakenBack};
use super::power::SCI_EN;
use super::{cpu, smi_cmd};
use crate::device::ClaimError;
use crate::isa::{self, Function};
use crate::log;
use crate::sync::{Lock, LockGuard};
use crate::time::{Cadence, Deadline, Duration};

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
/// How often the calls made for the holder are summed in the log.
const CALLS_SAID: Cadence = Cadence::every(
    Duration::from_secs(60),
    "said by the call that finds it due, so a holder that calls nothing says nothing, and one that storms says a line a minute",
);

struct Hardware {
    fixed: FixedHardware,
    control: Declared,
    /// Or why it is none a holder can be handed.
    ec: Result<Ec, String>,
    rsdp: u64,
    /// The window configuration space is reached through, as the MCFG bounds it.
    ecam: Option<Ecam>,
    lock: GlobalLock,
}

/// The firmware's Global Lock, as this machine's FADT has it.
#[derive(Clone, Copy)]
enum GlobalLock {
    /// The FADT names no FACS: the machine has no lock, and a take is
    /// answered taken.
    Absent,
    /// The FADT names a FACS this kernel exchanges no word in: a holder told
    /// it had the lock would hold one that excludes nothing, so a take is
    /// refused.
    Refused,
    At(Facs),
}

impl GlobalLock {
    fn facs(self) -> Option<Facs> {
        match self {
            Self::At(facs) => Some(facs),
            Self::Absent | Self::Refused => None,
        }
    }
}

/// The FACS, as `(start, end)`, and its lock word.
#[derive(Clone, Copy)]
struct Facs {
    span: (u64, u64),
    word: LockWordAt,
}

/// What this kernel does for the claim's holder.
struct Holder {
    /// The holder took the Global Lock and has not given it back.
    locked: bool,
    /// The holder supplied the power-off's sleep type: it supplies no second.
    supplied: bool,
    /// The calls into the firmware made for every holder there has been: a
    /// holder that dies and is started again begins no new second.
    calls: Calls,
}

/// The firmware calls made for the claim's holders, and what has been said
/// of them.
struct Calls {
    rate: CallRate,
    /// A bit a byte, set once a call of it has been said.
    said: [u64; 4],
    made: u64,
    spent: Duration,
    /// The call that held the boot processor longest, and its byte.
    longest: (Duration, u8),
    /// Refused past the rate.
    refused: u64,
    /// When the sum was last said, on the clock the rate is held on; none
    /// before the first call.
    summed_ns: Option<u64>,
}

/// Held across everything this kernel does for the claim's holder, from the
/// decision to the last instruction of the act: each mediated access, and
/// each change of the lock word. Taken with the claim's own lock held
/// (`object::Held`), and `pcidev`'s machine record and then `paging`'s record
/// of windows under it; nothing holding one of those two takes this or a
/// claim's.
static HOLDER: Lock<Holder> = Lock::new(Holder {
    locked: false,
    supplied: false,
    calls: Calls {
        rate: CallRate::new(),
        said: [0; 4],
        made: 0,
        spent: Duration::from_nanos(0),
        longest: (Duration::from_nanos(0), 0),
        refused: 0,
        summed_ns: None,
    },
});

/// The right to act for the claim's holder, held across the act; none once
/// the stop has begun. The claim is there for the whole of the act: its row
/// is lent under the lock its release takes the row with, and [`release`]
/// runs only once that has it.
fn acting(_claimed: &isa::Row) -> Result<LockGuard<'static, Holder>, SyscallError> {
    let holder = HOLDER.lock();
    if crate::quiesce::begun() {
        return Err(SyscallError::Gone);
    }
    Ok(holder)
}

/// PM1 control's `GBL_RLS` (ACPI 6.5 §4.8.3.2): written by the OS to tell the
/// firmware the Global Lock it asked for is free.
const GBL_RLS: u16 = 1 << 2;

/// Written once, by [`init`].
static HARDWARE: AtomicPtr<Hardware> = AtomicPtr::new(core::ptr::null_mut());

/// This kernel wrote `ACPI_ENABLE` and has not read `SCI_EN` clear since, so
/// the release writes `ACPI_DISABLE`.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Wait out whatever is being done for the claim's holder, and give back a
/// Global Lock it was stopped holding: the stop has begun, so no access or
/// take follows.
pub fn settle(_taken: &TakenBack) {
    let mut holder = HOLDER.lock();
    if let Some(hardware) = hardware() {
        give_back(hardware, &mut holder, "the machine is stopping");
    }
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
    if let Some(Err(why)) = fixed.smi_cmd.map(|named| smi_cmd::declare(named.port, named.named())) {
        return log!("acpi: no ACPI row — SMI_CMD not declared: {why:?}");
    }
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
    let (ecam, lock) = (ecam(rsdp_addr), global_lock(&fadt));
    let hardware = Hardware { fixed, control, ec, rsdp: rsdp_addr, ecam, lock };
    let was = HARDWARE.swap(Box::into_raw(Box::new(hardware)), Ordering::Release);
    assert!(was.is_null(), "acpi: init ran twice");
}

/// The ECAM window as the MCFG's first allocation bounds it (PCI Firmware
/// Specification 3.3, Table 4-3: the segment group at +8 of the entry, the
/// first and last bus at +10 and +11).
fn ecam(rsdp_addr: u64) -> Option<Ecam> {
    let (mcfg, base) = toyos_acpi::ecam_base(crate::drivers::acpi::direct_phys(), rsdp_addr).ok()?;
    let entry = toyos_acpi::MCFG_FIRST_ENTRY;
    let (segment, first_bus, last_bus) = (mcfg.u16_at(entry + 8)?, mcfg.byte(entry + 10)?, mcfg.byte(entry + 11)?);
    if first_bus > last_bus {
        log!("acpi: the MCFG's window ends at bus {last_bus:#x}, before its first, {first_bus:#x}: no configuration access is mediated");
        return None;
    }
    Some(Ecam { base, segment, first_bus, last_bus })
}

/// The Global Lock of the FACS the FADT names, said by name where there is
/// none or it is none this kernel takes: a FACS that does not decode, and one
/// whose lock word is not in memory the firmware's map gives the firmware.
fn global_lock<P: toyos_acpi::Phys>(fadt: &toyos_acpi::Table<P>) -> GlobalLock {
    let refused = |why: core::fmt::Arguments| {
        log!("acpi: a Global Lock this kernel cannot take — {why}: every take is refused");
        GlobalLock::Refused
    };
    let facs = match toyos_acpi::facs(fadt.phys(), fadt) {
        Ok(facs) => facs,
        Err(toyos_acpi::FacsRefused::Absent) => {
            log!("acpi: no Global Lock — the FADT names no FACS: every take is answered taken");
            return GlobalLock::Absent;
        }
        Err(why) => return refused(format_args!("the FADT's FACS is none this kernel reads ({why:?})")),
    };
    let map = crate::mm::firmware_map();
    let word = match firmware::lock_word(map, crate::mm::direct_map_end().get(), facs.base + toyos_acpi::FACS_GLOBAL_LOCK) {
        Ok(word) => word,
        Err(why) => return refused(format_args!("the lock word of the FADT's FACS at {:#x} is none this kernel exchanges ({why:?})", facs.base)),
    };
    log!(
        "acpi: the Global Lock is the FACS's at {:#x}, in memory the firmware's map types {}",
        word.at(),
        firmware::type_word(map, word.at())
    );
    GlobalLock::At(Facs { span: (facs.base, facs.base + u64::from(facs.len)), word })
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
pub fn claim() -> Result<(isa::Row, AcpiInfo), ClaimError> {
    let hardware = hardware().ok_or(ClaimError::Absent)?;
    // Released by the refusal's return, which drops it.
    let row = isa::claim_row(ROW)?;
    enter(hardware)?;
    Ok((row, info(hardware)))
}

fn info(hardware: &Hardware) -> AcpiInfo {
    let (ec_command, ec_data, ec_gpe) = match &hardware.ec {
        Ok(ec) => (Block { port: ec.command, len: 1 }, Block { port: ec.data, len: 1 }, u16::from(ec.gpe)),
        Err(_) => (Block::NONE, Block::NONE, 0),
    };
    AcpiInfo {
        rsdp: hardware.rsdp,
        pm1_event: hardware.fixed.pm1a_event,
        gpe0: hardware.fixed.gpe0,
        ec_command,
        ec_data,
        ec_gpe,
        flags: if hardware.fixed.power_button == PowerButton::Fixed { FIXED_POWER_BUTTON } else { 0 },
        reserved: 0,
    }
}

/// `SMI_CMD` and the way out of legacy mode and back, where the FADT names
/// all three.
fn legacy(hardware: &Hardware) -> Option<(u16, LegacyMode)> {
    let named = hardware.fixed.smi_cmd?;
    Some((named.port, named.legacy()?))
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
    let Some((port, legacy)) = legacy(hardware) else {
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

    let Some(write) = smi_cmd::write(enable) else {
        return refuse("the machine is stopping");
    };
    ENABLED.store(true, Ordering::Relaxed);
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
        "acpi: ACPI mode: ACPI_ENABLE {enable:#04x} written to SMI_CMD {port:#x} {write}; SCI_EN set {} after",
        crate::clock::now() - written,
    );
    Ok(())
}

/// The claim's last handle went: the machine back in legacy mode where the
/// mint took it out of it. Before the row's own release, which its caller's
/// drop of the row is, so no claimant finds `SCI_EN` set by a holder whose
/// disable is still to come.
pub fn release() {
    let hardware = hardware().expect("a claimed row has its hardware");
    {
        let mut holder = HOLDER.lock();
        // The next claim's holder supplies its own; this one's stands until then.
        holder.supplied = false;
        give_back(hardware, &mut holder, "its claim is gone");
    }
    if ENABLED.load(Ordering::Relaxed) {
        leave(hardware);
    }
}

/// `ACPI_DISABLE`, written where the mint wrote `ACPI_ENABLE`, and `SCI_EN`
/// read until it is clear; nothing once the stop has begun. Spun, never
/// parked: the task a claim's last handle goes with may be dying. `SCI_EN` is
/// the hardware's to reset (ACPI 6.5 §4.8.2.5, Table 4.13), so it is not
/// cleared here before the write as Table 5.9's `ACPI_DISABLE` has it.
fn leave(hardware: &Hardware) {
    let (port, legacy) = legacy(hardware).expect("ACPI_ENABLE was written to it");
    let disable = legacy.acpi_disable.get();
    let Some(write) = smi_cmd::write(disable) else {
        return log!("acpi: ACPI_DISABLE not written: the machine is stopping, and its power-off owns ACPI mode");
    };
    let written = crate::clock::now();
    let by = Deadline::at(written + HANDBACK);
    let control = loop {
        let control = cpu::inw(hardware.control.port(0));
        if control & SCI_EN == 0 {
            break control;
        }
        if by.reached(crate::clock::now()) {
            return log!(
                "acpi: still in ACPI mode: ACPI_DISABLE {disable:#04x} written to SMI_CMD {port:#x} {write}; PM1a_CNT reads \
                 {control:#06x} {HANDBACK} after, SCI_EN still set: nothing serves this machine's buttons until a holder \
                 claims them"
            );
        }
        core::hint::spin_loop();
    };
    ENABLED.store(false, Ordering::Relaxed);
    log!(
        "acpi: legacy mode again: ACPI_DISABLE {disable:#04x} written to SMI_CMD {port:#x} {write}; PM1a_CNT reads \
         {control:#06x} {} after, SCI_EN clear",
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

/// The FACS's lock word. One `AtomicU32` and nothing else of the page: the
/// firmware's SMI handlers change it under this kernel.
fn lock_word(word: LockWordAt) -> &'static AtomicU32 {
    let at = crate::mm::DirectMap::from_phys(word.at());
    // SAFETY: the policy passed the word: on a dword boundary, all four bytes
    // inside the direct map and in memory the firmware's map gives the
    // firmware, so it is mapped for the machine's life and no Rust object.
    unsafe { &*at.as_ptr::<AtomicU32>() }
}

/// Try the Global Lock for the claim's holder: `Ok(true)` taken,
/// `Ok(false)` where the firmware owns it, with the pending bit left set for
/// the firmware's release to answer with `GBL_STS`, and `NotSupported` on a
/// machine whose lock this kernel cannot take.
pub fn lock_take(row: &isa::Row) -> Result<bool, SyscallError> {
    let hardware = hardware().expect("a claimed row has its hardware");
    let mut holder = acting(row)?;
    if holder.locked {
        return Err(SyscallError::AlreadyExists);
    }
    let taken = match hardware.lock {
        GlobalLock::Absent => true,
        GlobalLock::Refused => return Err(SyscallError::NotSupported),
        GlobalLock::At(facs) => {
            let word = lock_word(facs.word);
            let mut read = word.load(Ordering::Acquire);
            loop {
                let (new, acquired) = toyos_acpi::acquire(read);
                match word.compare_exchange(read, new, Ordering::AcqRel, Ordering::Acquire) {
                    Ok(_) => break acquired,
                    Err(now) => read = now,
                }
            }
        }
    };
    holder.locked = taken;
    Ok(taken)
}

/// Give the Global Lock back for the claim's holder.
pub fn lock_release(row: &isa::Row) -> Result<(), SyscallError> {
    let hardware = hardware().expect("a claimed row has its hardware");
    let mut holder = acting(row)?;
    if !holder.locked {
        return Err(SyscallError::InvalidArgument);
    }
    give_back(hardware, &mut holder, "");
    Ok(())
}

/// Take the `SLP_TYPa` of the machine's `\_S5` from the claim's holder, for
/// the power-off (`power::supply`): `InvalidArgument` is a word wider than
/// the register's field, and `AlreadyExists` a second one under this claim;
/// nothing is kept of either. Its line is said under the claim's lock and
/// [`HOLDER`]: `log::emit` takes no lock.
pub fn s5(row: &isa::Row, word: u64) -> Result<(), SyscallError> {
    let slp_typ = firmware::sleep_type(word).ok_or(SyscallError::InvalidArgument)?;
    let mut holder = acting(row)?;
    if holder.supplied {
        return Err(SyscallError::AlreadyExists);
    }
    holder.supplied = true;
    super::power::supply(slp_typ);
    Ok(())
}

/// Clear the lock word's owner where the claim's holder holds it, and tell
/// the firmware where it asked meanwhile. `orphaned` says why the holder did
/// not give it back itself, for the log; empty where it did.
fn give_back(hardware: &Hardware, holder: &mut Holder, orphaned: &str) {
    if !core::mem::take(&mut holder.locked) {
        return;
    }
    let signalled = hardware.lock.facs().is_some_and(|facs| {
        let word = lock_word(facs.word);
        let mut read = word.load(Ordering::Acquire);
        let signal = loop {
            let (new, signal) = toyos_acpi::release(read);
            match word.compare_exchange(read, new, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break signal,
                Err(now) => read = now,
            }
        };
        if signal {
            let control = hardware.control.port(0);
            // SAFETY: the PM1a control block, declared; `GBL_RLS` over the
            // register as it reads, whose `SLP_EN` reads clear (§4.8.3.2).
            unsafe { cpu::outw(control, cpu::inw(control) | GBL_RLS) };
        }
        signal
    });
    if !orphaned.is_empty() {
        log!(
            "acpi: the Global Lock given back for a holder that left it taken ({orphaned}){}",
            if signalled { ", and the firmware, which asked for it meanwhile, told by GBL_RLS" } else { "" }
        );
    }
}

/// `debug_action::ACPI_FIRMWARE_LOCK`: the firmware's side of the lock word.
#[cfg(feature = "test-actuators")]
pub fn debug_firmware_lock(act: u64) -> u64 {
    use toyos_abi::syscall::debug_action::{FIRMWARE_ASKS, FIRMWARE_FREES, FIRMWARE_OWNS};
    let Some(facs) = hardware().and_then(|hardware| hardware.lock.facs()) else { return SyscallError::NotSupported.to_u64() };
    let word = lock_word(facs.word);
    let was = match act {
        FIRMWARE_FREES => word.fetch_and(!(toyos_acpi::OWNED | toyos_acpi::PENDING), Ordering::AcqRel),
        FIRMWARE_OWNS => word.fetch_or(toyos_acpi::OWNED, Ordering::AcqRel),
        FIRMWARE_ASKS => word.fetch_or(toyos_acpi::PENDING, Ordering::AcqRel),
        _ => return SyscallError::InvalidArgument.to_u64(),
    };
    u64::from(was)
}

/// Bytes at an address whatever its alignment, moved by one instruction.
#[repr(C, packed)]
struct Unaligned<T>(T);

fn read_memory(passed: &MemoryAt) -> u64 {
    let at = crate::mm::DirectMap::from_phys(passed.at());
    // SAFETY: the policy passed the range as the firmware's, inside the direct
    // map and in no memory this kernel hands out: mapped for the machine's
    // life, and no Rust object.
    unsafe {
        match passed.width() {
            Width::Byte => u64::from(at.as_ptr::<u8>().read_volatile()),
            Width::Word => u64::from(at.as_ptr::<Unaligned<u16>>().read_volatile().0),
            Width::DWord => u64::from(at.as_ptr::<Unaligned<u32>>().read_volatile().0),
            Width::QWord => at.as_ptr::<Unaligned<u64>>().read_volatile().0,
        }
    }
}

fn write_memory(passed: &MemoryAt, value: u64) {
    let at = crate::mm::DirectMap::from_phys(passed.at());
    // SAFETY: as `read_memory`, and the policy passed the write: the firmware's
    // own reserved or non-volatile memory, outside its tables and the FACS.
    unsafe {
        match passed.width() {
            Width::Byte => at.as_mut_ptr::<u8>().write_volatile(value as u8),
            Width::Word => at.as_mut_ptr::<Unaligned<u16>>().write_volatile(Unaligned(value as u16)),
            Width::DWord => at.as_mut_ptr::<Unaligned<u32>>().write_volatile(Unaligned(value as u32)),
            Width::QWord => at.as_mut_ptr::<Unaligned<u64>>().write_volatile(Unaligned(value)),
        }
    }
}

fn read_port(passed: &PortAt) -> u64 {
    let port = pio::mediated(passed);
    match passed.width() {
        Width::Byte => u64::from(cpu::inb(port)),
        Width::Word => u64::from(cpu::inw(port)),
        Width::DWord => u64::from(cpu::inl(port)),
        Width::QWord => unreachable!("the policy passes no qword port access"),
    }
}

fn write_port(passed: &PortAt, value: u64) {
    let port = pio::mediated(passed);
    // SAFETY: the policy passed a write to every port of the span: none this
    // kernel declared and keeps, and none another claim's row names.
    unsafe {
        match passed.width() {
            Width::Byte => cpu::outb(port, value as u8),
            Width::Word => cpu::outw(port, value as u16),
            Width::DWord => cpu::outl(port, value as u32),
            Width::QWord => unreachable!("the policy passes no qword port access"),
        }
    }
}

/// One configuration access, decided and, where it is a read, made through
/// the window the MCFG names, which the policy bounded the function by.
fn config(hardware: &Hardware, _acting: &Holder, segment: u16, function: PciFunction, offset: u16, width: Width, write: bool) -> Result<u64, Refused> {
    let at = firmware::config(hardware.ecam, segment, function, offset, width, write)?;
    let PciFunction { bus, device, function } = at.function();
    let space = crate::drivers::pci::function_window(bus, device, function).expect("a machine with a claimable ACPI row enumerated its PCI functions");
    let offset = u64::from(at.offset());
    Ok(match at.width() {
        Width::Byte => u64::from(space.read_u8(offset)),
        Width::Word => u64::from(space.read_u16(offset)),
        Width::DWord => u64::from(space.read_u32(offset)),
        Width::QWord => unreachable!("the policy passes no qword configuration access"),
    })
}

/// Whether a read of `len` bytes at `at` through the direct map is uncached:
/// its leaves select the PAT's write-back entry, under which the range
/// registers decide (Intel SDM Vol. 3A, Table 12-7). The boot processor's
/// decide it, as read at boot, so the answer is one whichever CPU asks: a
/// CPU whose own are off answers nothing of what firmware typed the range,
/// and reads it uncached all the same.
fn uncached(at: u64, len: u64) -> bool {
    let (def_type, pairs) = super::mtrr::boot();
    kernel::mtrr::range_type(def_type, pairs.iter().copied(), at, at + (len - 1)).typed_uncacheable()
}

/// One memory access, decided and made; the type firmware's map gives its
/// first byte goes back with either. The records of what devices decode are
/// read under their own locks and let go before the access: a window mapped
/// after the decision is one the access was made a moment before.
fn memory(hardware: &Hardware, acting: &Holder, request: &mut Access, width: Width, write: Option<u64>) -> Result<u64, Refused> {
    let map = crate::mm::firmware_map();
    request.memory_type = firmware::type_word(map, request.address);
    let at = request.address;
    let verdict = crate::pcidev::with_bar_memory(|bars| {
        crate::mm::paging::with_driven_windows(|driven| {
            let memory = Memory {
                map,
                mapped_end: crate::mm::direct_map_end().get(),
                ecam: hardware.ecam,
                devices: driven.iter().copied().chain(bars),
                facs: hardware.lock.facs().map(|facs| facs.span),
                uncached,
                registers_differ: super::mtrr::any_differs(),
            };
            memory.decide(at, width, write.is_some())
        })
    });
    match (verdict, write) {
        (MemoryVerdict::Through(passed), None) => Ok(read_memory(&passed)),
        (MemoryVerdict::Through(passed), Some(value)) => {
            write_memory(&passed, value);
            Ok(0)
        }
        (MemoryVerdict::AsConfig(function, offset), _) => {
            let segment = hardware.ecam.expect("the policy answered a configuration access from an ECAM window").segment;
            config(hardware, acting, segment, function, offset, width, write.is_some())
        }
        (MemoryVerdict::Refused(refused), _) => Err(refused),
    }
}

/// Why an access was not made: the policy's refusal, which goes back in the
/// request, or the stop, which begun after the claim's holder was let act.
enum Unmade {
    Refused(Refused),
    Stopping,
}

impl From<Refused> for Unmade {
    fn from(refused: Refused) -> Self {
        Self::Refused(refused)
    }
}

/// One port access, decided and made.
fn port(hardware: &Hardware, acting: &mut Holder, address: u64, width: Width, write: Option<u64>) -> Result<u64, Unmade> {
    let port = u16::try_from(address).map_err(|_| Refused::PortSpan)?;
    match (firmware::port(|port| pio::standing(port, ROW), port, width, write), write) {
        (PortVerdict::Through(passed), None) => Ok(read_port(&passed)),
        (PortVerdict::Through(passed), Some(value)) => {
            write_port(&passed, value);
            Ok(0)
        }
        (PortVerdict::FirmwareCall(asked), _) => call(hardware, &mut acting.calls, asked),
        (PortVerdict::Refused(refused), _) => Err(refused.into()),
    }
}

/// Make the call into the firmware the policy passed, on the boot processor,
/// or refuse it past the rate; returned from once the firmware's handler has.
/// The first call of each byte is said with what the boot processor read
/// around it, and after that the sum of them every [`CALLS_SAID`].
fn call(hardware: &Hardware, calls: &mut Calls, asked: FirmwareCall) -> Result<u64, Unmade> {
    let now = crate::clock::nanos_since_boot();
    let value = asked.value();
    let made = calls.rate.admit(now);
    if made {
        let written = smi_cmd::write(value).ok_or(Unmade::Stopping)?;
        calls.made += 1;
        calls.spent = Duration::from_nanos(calls.spent.nanos() + written.held().nanos());
        if written.held() > calls.longest.0 {
            calls.longest = (written.held(), value);
        }
        let (word, bit) = (&mut calls.said[usize::from(value / 64)], 1u64 << (value % 64));
        if *word & bit == 0 {
            *word |= bit;
            let port = hardware.fixed.smi_cmd.expect("the policy passed a call to the SMI_CMD the FADT names").port;
            log!("acpi: firmware call {value:#04x} written to SMI_CMD {port:#x} {written}; the first of that byte");
        }
    } else {
        calls.refused += 1;
    }
    if now >= calls.summed_ns.get_or_insert(now).saturating_add(CALLS_SAID.nanos()) {
        calls.summed_ns = Some(now);
        log!(
            "acpi: firmware calls made for the claim's holder: {}, which held the boot processor {} in all and {} at the longest, \
             for {:#04x}; {} refused past {} a second",
            calls.made,
            calls.spent,
            calls.longest.0,
            calls.longest.1,
            calls.refused,
            firmware::CALLS,
        );
    }
    if made { Ok(0) } else { Err(Refused::CommandRate.into()) }
}

/// Make the access `request` names for the holder of the claim that lends
/// `row`, or refuse it by name: a read's value, the refusal and the memory
/// type are written back into it. `Err` is a request that names no space,
/// width or direction, a value wider than its width, or a reserved byte that
/// is not zero; and `Gone` once the stop has begun, whether before the access
/// or under a call into the firmware it asked for.
pub fn access(row: &isa::Row, request: &mut Access) -> Result<(), SyscallError> {
    let hardware = hardware().expect("a claimed row has its hardware");
    let (Some(space), Some(width)) = (Space::from_raw(request.space), Width::from_raw(request.width)) else {
        return Err(SyscallError::InvalidArgument);
    };
    let write = match request.write {
        0 => None,
        1 if request.value <= width.max_value() => Some(request.value),
        _ => return Err(SyscallError::InvalidArgument),
    };
    if request.reserved != [0; 3] {
        return Err(SyscallError::InvalidArgument);
    }
    let mut acting = acting(row)?;
    request.memory_type = toyos_abi::acpi::UNLISTED;
    let made = match space {
        Space::SystemMemory => memory(hardware, &acting, request, width, write).map_err(Unmade::from),
        Space::SystemIo => port(hardware, &mut acting, request.address, width, write),
        Space::PciConfig => {
            // `toyos_abi::acpi::pci_address`: nothing above the segment group.
            let at = request.address;
            let function = PciFunction { bus: (at >> 24) as u8, device: (at >> 19 & 0x1F) as u8, function: (at >> 16 & 7) as u8 };
            match at >> 48 {
                0 => config(hardware, &acting, (at >> 32) as u16, function, at as u16, width, write.is_some()).map_err(Unmade::from),
                _ => Err(Refused::ConfigUnreachable.into()),
            }
        }
    };
    match made {
        Ok(value) => {
            request.refused = 0;
            if write.is_none() {
                request.value = value;
            }
        }
        Err(Unmade::Refused(refused)) => request.refused = refused as u8,
        Err(Unmade::Stopping) => return Err(SyscallError::Gone),
    }
    Ok(())
}
