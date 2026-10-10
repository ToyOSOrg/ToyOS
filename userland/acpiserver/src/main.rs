//! `/system/bin/acpiserver`: the machine's ACPI fixed hardware, served.
//!
//! The kernel puts the machine in ACPI mode when it mints this program's
//! `acpi` claim, and back in the mode its firmware handed over when the claim
//! goes with this process (`toyos_abi::acpi`). From then on every power
//! management event the firmware served in legacy mode is this server's, and
//! there is no restart: a fault ends it loudly, and the firmware has them
//! again.
//!
//! **At start** every fixed and general-purpose event is disabled and its
//! status cleared, then the fixed power button is enabled where the FADT
//! says the machine has one, and the SCI acknowledged: a press after the
//! clear latches and is served, one before it is lost.
//!
//! **Then the machine's tables are loaded** ([`aml::load`]), through the
//! kernel's mediated access ([`Claim`]): after the arming, so a press during
//! the load latches and is served when it ends. A table refused, and a DSDT
//! refused, are each said and survived; the power button is served either
//! way. The load hands the kernel `\_S5`'s sleep type, without which the
//! kernel refuses every power-off.
//!
//! **Then the embedded controller is found in the namespace and armed**
//! ([`devices::find`]): its GPE enabled and its backlog drained. Its two
//! ports are no row's, so every access to them is the kernel's to make
//! ([`Claim`]). A machine whose DSDT did not load has no controller served.
//! **Where the FADT says the power button is a control method device**, the
//! buttons the namespace names are found too, and each query is run as its
//! method ([`devices::query`]): a Notify of a press is a press. Elsewhere no
//! query's method runs.
//!
//! **Each SCI** is read off both blocks ([`sci::events`]): a press stops the
//! machine through the supervisor, or is said and dropped on a machine with
//! no power-off, and the controller's GPE drains the
//! controller of every query waiting, which are then run, one by one, after
//! the drain that took them. An event this server
//! never enabled, a controller that does not answer, more queries in one
//! drain than [`QUERIES`], and [`sci::EMPTY_SCIS`] SCIs in a row that carried
//! nothing are each a panic naming the registers. Inside an evaluation, a
//! take of the Global Lock the firmware holds waits on the SCI for its
//! release alone ([`sci::await_release`]), and what else latched meanwhile is
//! served after.
//!
//! **After the load the machine's batteries and AC adapters are found** in
//! the namespace it kept, and read every [`battery::POLL`] between SCIs
//! ([`battery`]): the controller's space is reached only there, never inside
//! a drain.
//!
//! The log carries each query number the first time it is taken and a count
//! of every one at [`COUNTS`] intervals, never a line per event.

mod aml;
mod battery;
mod devices;
mod ec;
mod host;
mod ledger;
mod sci;
mod tables;

use std::cell::Cell;
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use toyos::endow;
use toyos::ioport::{in16, in8, out16, out8};
use toyos::poller::{Poller, READABLE};
use toyos::power::{self, Stop};
use toyos::AcpiDev;
use toyos_abi::acpi::{Access, AcpiInfo, Block, Space, Width, FIXED_POWER_BUTTON};
use toyos_abi::syscall::{DeviceType, SyscallError};

use acpiserver_api::{CONTROLLER_NONE, CONTROLLER_SERVED};
use aml::Aml;
use devices::{Ec, Queried};
use ec::{Do, Transaction, Wait};
use battery::Power;
use host::{Answer, Controller, Kernel, Stopping, Take};
use sci::{Enables, Event, Fixed, Served, Unserved, Unstopped, PM1_STATUS, PWRBTN};

/// The most a controller is waited for at one step of a transaction: one that
/// has not moved in this will not.
const EC_STEP: Duration = Duration::from_millis(500);
/// The most queries one drain takes before the controller is a storm.
const QUERIES: usize = 32;
/// How often the log is told the queries' counts, where they moved.
const COUNTS: Duration = Duration::from_secs(30);

struct Server<'a> {
    claim: &'a Claim<'a>,
    info: AcpiInfo,
    served: Served,
    /// The controller the namespace names, if it names one this server serves.
    ec: Option<Ec>,
    /// The namespace the load kept, and the power sources found in it.
    aml: Option<Aml<'a, Claim<'a>, Ports<'a>>>,
    power: Option<Power>,
    /// Taken off the controller by the drain, run after it.
    queued: VecDeque<u8>,
    /// Every query number taken, and how often.
    counts: BTreeMap<u8, u64>,
    counted: u64,
    logged: u64,
    scis: u64,
    empty: u32,
    /// The kernel was handed `\_S5`'s sleep type: a press powers the machine off.
    power_off: bool,
}

fn main() {
    let Some(dev) = endow::device::<AcpiDev>(DeviceType::Acpi) else {
        println!("acpiserver: no acpi claim, so the machine stays in the mode its firmware handed over");
        return;
    };
    let info = dev.describe().expect("acpiserver: the claim's first read is its description");
    let served = Served { power_button: info.flags & FIXED_POWER_BUTTON != 0, ec_gpe: None };
    let claim = Claim { dev: &dev, info, poller: Poller::new(1), watching: Cell::new(false) };
    let mut server = Server {
        claim: &claim,
        info,
        served,
        ec: None,
        aml: None,
        power: None,
        queued: VecDeque::new(),
        counts: BTreeMap::new(),
        counted: 0,
        logged: 0,
        scis: 0,
        empty: 0,
        power_off: false,
    };
    server.arm();
    let (loaded, kept) = aml::load(&claim, None, server.info.rsdp);
    server.power_off = loaded.handed;
    server.aml = kept;
    server.arm_controller();
    if let Some(aml) = &mut server.aml {
        server.power = Power::find(aml, server.ec.is_some());
    }
    server.serve();
}

/// The embedded controller's two ports, reached through the kernel's
/// mediated access: no row holds them.
#[derive(Clone, Copy)]
struct Ports<'a> {
    claim: &'a Claim<'a>,
    command: u16,
    data: u16,
}

impl Controller for Ports<'_> {
    fn transact(&mut self, tx: Transaction) -> Result<u8, Stopping> {
        transact(self.claim, self.command, self.data, tx).ok_or(Stopping)
    }
}

/// The claim as the tables' fetch and their AML ask it for what lies outside
/// its own ports, and as the wait for the firmware's release of the Global
/// Lock reads its event blocks and its SCI; and the one poller its SCI is
/// waited on with, between SCIs and inside that wait.
struct Claim<'a> {
    dev: &'a AcpiDev,
    info: AcpiInfo,
    poller: Poller,
    /// A watch answers once: one still registered from a wait that timed out
    /// is not registered again.
    watching: Cell<bool>,
}

impl Claim<'_> {
    /// Waits until `until` for the claim's record to be readable.
    fn readable(&self, until: Instant) {
        if !self.watching.replace(true) {
            self.poller.watch(self.dev, READABLE, 0);
        }
        let wait = until.saturating_duration_since(Instant::now());
        self.poller.wait(1, wait.as_nanos() as u64, |_| self.watching.set(false));
    }

    /// The kernel's answer; a stopping machine's is the caller's to carry,
    /// and any other refusal of a call this server formed is this server's
    /// defect.
    fn answered<T>(asked: &str, answer: Result<T, SyscallError>) -> Result<T, Stopping> {
        match answer {
            Ok(answer) => Ok(answer),
            Err(SyscallError::Gone) => Err(Stopping),
            Err(other) => panic!("acpiserver: the kernel answered {asked} {other:?}"),
        }
    }
}

impl Kernel for Claim<'_> {
    fn access(&self, access: Access) -> Result<Answer, Stopping> {
        Self::answered("a mediated access", self.dev.access(access)).map(|(made, memory_type)| Answer { made, memory_type })
    }

    fn lock_take(&self) -> Result<Take, Stopping> {
        match self.dev.lock_take() {
            Err(SyscallError::NotSupported) => Ok(Take::Unusable),
            answer => Self::answered("a take of the Global Lock", answer).map(|taken| if taken { Take::Taken } else { Take::Pending }),
        }
    }

    fn lock_release(&self) -> Result<(), Stopping> {
        Self::answered("the Global Lock's release", self.dev.lock_release())
    }

    fn released(&self, until: Instant) -> bool {
        sci::await_release(&mut Waiting(self), until)
    }

    fn s5(&self, slp_typ_a: u64) -> Result<bool, Stopping> {
        match self.dev.s5(slp_typ_a) {
            Err(SyscallError::InvalidArgument) => Ok(false),
            answer => Self::answered("the power-off's sleep type", answer).map(|()| true),
        }
    }
}

/// The claim's event blocks and SCI for one wait for the firmware's release.
struct Waiting<'c, 'a>(&'c Claim<'a>);

impl Fixed for Waiting<'_, '_> {
    fn pm1_status(&mut self) -> u16 {
        in16(self.0.info.pm1_event.port)
    }

    fn pm1_clear(&mut self, bits: u16) {
        out16(self.0.info.pm1_event.port, bits);
    }

    fn enables(&mut self) -> Enables {
        Enables {
            pm1: in16(self.0.info.pm1_event.enable()),
            gpe0: bytes(self.0.info.gpe0).map(|(_, enable)| in8(enable)).collect(),
        }
    }

    fn enable(&mut self, enables: &Enables) {
        out16(self.0.info.pm1_event.enable(), enables.pm1);
        for ((_, enable), &byte) in bytes(self.0.info.gpe0).zip(&enables.gpe0) {
            out8(enable, byte);
        }
    }

    fn sci(&mut self, until: Instant) -> bool {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        self.0.readable(until);
        match self.0.dev.irq() {
            Ok(_) | Err(SyscallError::WouldBlock) => true,
            Err(other) => panic!("acpiserver: the claim's record answered {other:?} while the Global Lock was waited for"),
        }
    }

    fn ack(&mut self) {
        self.0.dev.ack().expect("acpiserver: the claim's acknowledgement");
    }
}

impl Claim<'_> {
    /// A byte of one of the controller's ports, which no row holds: `None`
    /// once the machine is stopping, and a refusal is the kernel's keeping a
    /// port [`Server::arm_controller`] found it would reach.
    fn port_in(&self, port: u16) -> Option<u8> {
        let answer = self.access(Access::read(Space::SystemIo, u64::from(port), Width::Byte)).ok()?;
        Some(answer.made.unwrap_or_else(|refused| panic!("acpiserver: the kernel refused a read of the embedded controller's port {port:#x}: {refused:?}")) as u8)
    }

    fn port_out(&self, port: u16, byte: u8) -> Option<()> {
        let answer = self.access(Access::write(Space::SystemIo, u64::from(port), Width::Byte, u64::from(byte))).ok()?;
        answer.made.unwrap_or_else(|refused| panic!("acpiserver: the kernel refused a write of the embedded controller's port {port:#x}: {refused:?}"));
        Some(())
    }
}

/// Each byte of a status-and-enable block: its status port and its enable port.
fn bytes(block: Block) -> impl Iterator<Item = (u16, u16)> {
    (0..block.len / 2).map(move |i| (block.port + i, block.enable() + i))
}

impl Server<'_> {
    fn arm(&mut self) {
        let pm1 = self.info.pm1_event;
        out16(pm1.enable(), 0);
        out16(pm1.port, PM1_STATUS);
        for (status, enable) in bytes(self.info.gpe0) {
            out8(enable, 0);
            out8(status, 0xFF);
        }
        if self.served.power_button {
            out16(pm1.enable(), PWRBTN);
        }
        self.claim.dev.ack().expect("acpiserver: the claim's acknowledgement");
        println!(
            "acpiserver: armed: power button {}",
            if self.served.power_button { "served" } else { "a control method device, served once the namespace names it" },
        );
    }

    /// Find the controller, and the buttons where they are control method
    /// devices, in the namespace the load kept; enable the controller's GPE
    /// and drain its backlog.
    fn arm_controller(&mut self) {
        let Some(aml) = &mut self.aml else {
            if !self.served.power_button {
                println!("acpiserver: no control-method power button served: no namespace was kept");
            }
            return println!("{CONTROLLER_NONE}no namespace was kept");
        };
        let found = devices::find(&mut aml.interpreter, &mut aml.host, self.info.gpe0, !self.served.power_button);
        if !self.served.power_button {
            println!("acpiserver: {} control-method power button(s) served, each by the queries' Notify", found.buttons.len());
        }
        aml.host.buttons = found.buttons;
        let Some(ec) = found.ec else { return };
        let claim = self.claim;
        // Its status register, which a read leaves as it was; the data
        // register's read takes a byte, and a refusal of it is the panic of
        // `Claim::port_in`.
        match claim.access(Access::read(Space::SystemIo, u64::from(ec.command), Width::Byte)) {
            Err(Stopping) => return,
            Ok(Answer { made: Err(refused), .. }) => return println!("{CONTROLLER_NONE}the kernel keeps its status port from this server, {refused:?}"),
            Ok(Answer { made: Ok(_), .. }) => {}
        }
        let n = ec.gpe;
        let (status, enable) = bytes(self.info.gpe0).nth(usize::from(n / 8)).expect("the controller's GPE was bounded by the block");
        out8(status, 1 << (n % 8));
        out8(enable, in8(enable) | 1 << (n % 8));
        println!("{CONTROLLER_SERVED}{n:#x} at {:#x}/{:#x}", ec.command, ec.data);
        self.served.ec_gpe = Some(n);
        aml.host.ec = Some(Ports { claim, command: ec.command, data: ec.data });
        self.ec = Some(ec);
        self.drain();
        self.run_queued();
    }

    fn serve(&mut self) -> ! {
        let mut next_count = Instant::now() + COUNTS;
        let mut next_read = Instant::now() + battery::POLL;
        loop {
            if let (Some(power), Some(aml)) = (&mut self.power, &mut self.aml)
                && Instant::now() >= next_read
            {
                for line in power.read(aml) {
                    println!("{line}");
                }
                next_read = Instant::now() + battery::POLL;
                if aml.host.stopping {
                    self.power = None;
                }
            }
            self.claim.readable(if self.power.is_some() { next_count.min(next_read) } else { next_count });
            if Instant::now() >= next_count {
                self.log_counts();
                next_count = Instant::now() + COUNTS;
            }
            match self.claim.dev.irq() {
                Ok(record) => self.scis += u64::from(record.count),
                Err(SyscallError::WouldBlock) => continue,
                Err(other) => panic!("acpiserver: the claim's record answered {other:?}"),
            }
            self.take();
            self.claim.dev.ack().expect("acpiserver: the claim's acknowledgement");
        }
    }

    /// Everything one SCI carried, each event served and its status cleared.
    fn take(&mut self) {
        let pm1 = self.info.pm1_event;
        let gpe0: Vec<(u8, u8)> = bytes(self.info.gpe0).map(|(status, enable)| (in8(status), in8(enable))).collect();
        let events = match sci::events(&self.served, (in16(pm1.port), in16(pm1.enable())), &gpe0) {
            Ok(events) => events,
            Err(Unserved::Pm1 { status, enable }) => panic!(
                "acpiserver: PM1 status {status:#06x} under enable {enable:#06x} carries an event this server never enabled"
            ),
            Err(Unserved::Gpe(n)) => panic!("acpiserver: GPE {n:#x} is enabled and set, and this server never enabled it"),
        };
        if events.is_empty() {
            self.empty += 1;
            assert!(
                self.empty < sci::EMPTY_SCIS,
                "acpiserver: {} SCIs in a row carried no event this server serves (PM1 {:#06x}, GPE0 {gpe0:x?})",
                self.empty,
                in16(pm1.port),
            );
            return;
        }
        self.empty = 0;
        for event in events {
            match event {
                Event::PowerButton => {
                    out16(pm1.port, PWRBTN);
                    self.press();
                }
                Event::Ec => {
                    // The controller's GPE is an edge: cleared before the drain,
                    // so an event the drain does not see raises it again.
                    let n = self.served.ec_gpe.expect("an event of the controller's GPE is one this server enabled");
                    let (status, _) = bytes(self.info.gpe0).nth(usize::from(n / 8)).expect("the controller's GPE was bounded by the block");
                    out8(status, 1 << (n % 8));
                    self.drain();
                }
            }
        }
        self.run_queued();
    }

    /// A press: the machine stops, and this comes back only where it has no
    /// power-off, which the load said and this says again.
    fn press(&mut self) {
        println!("acpiserver: the power button was pressed, on SCI {} of this boot; asking the supervisor to power off", self.scis);
        let refused = power::stop(Stop::Shutdown);
        match sci::unstopped(refused, self.power_off) {
            Unstopped::Dropped => toyos::error!("acpiserver: the press is dropped: {}", aml::NO_S5_HANDED.trim_start_matches("acpiserver: ")),
            Unstopped::Defect => panic!("acpiserver: the power-off was refused: {refused:?}"),
        }
    }

    /// Take every query the controller has waiting off it, queued for after.
    fn drain(&mut self) {
        let ec = self.ec.as_ref().expect("a drain runs only where a controller is served");
        let (claim, command, data) = (self.claim, ec.command, ec.data);
        let mut taken = 0;
        while let Some(status) = claim.port_in(command)
            && status & ec::SCI_EVT != 0
        {
            let Some(q) = transact(claim, command, data, Transaction::query()) else { return };
            if q == 0 {
                break;
            }
            taken += 1;
            assert!(taken <= QUERIES, "acpiserver: the embedded controller had more than {QUERIES} queries waiting at once (EC_SC {status:#04x})");
            self.queued.push_back(q);
        }
    }

    /// Each query the drain took, run as its method where the buttons are
    /// control method devices, and counted.
    fn run_queued(&mut self) {
        while let Some(q) = self.queued.pop_front() {
            let served = match (&mut self.aml, &self.ec) {
                (Some(aml), Some(ec)) if !aml.host.buttons.is_empty() => {
                    let queried = devices::query(&mut aml.interpreter, &mut aml.host, ec, q);
                    let presses = std::mem::take(&mut aml.host.presses);
                    Some((queried, presses))
                }
                _ => None,
            };
            let count = self.counts.entry(q).or_insert(0);
            *count += 1;
            self.counted += 1;
            if *count == 1 {
                let said = match &served {
                    None => "served by nothing: no query's method runs on a machine whose power button is the fixed one".into(),
                    Some((Queried::Ran, _)) => "its method ran".into(),
                    Some((Queried::Absent, _)) => "the controller defines no method for it".into(),
                    Some((Queried::Refused(why), _)) => format!("its method did not finish: {why}"),
                };
                println!("acpiserver: embedded controller query {q:#04x} taken for the first time; {said}");
            }
            if served.is_some_and(|(_, presses)| presses != 0) {
                self.press();
            }
        }
    }

    fn log_counts(&mut self) {
        if self.counted == self.logged {
            return;
        }
        self.logged = self.counted;
        let counts: Vec<String> = self.counts.iter().map(|(q, n)| format!("{q:#04x} x{n}")).collect();
        println!("acpiserver: {} SCIs; {}{}", self.scis, acpiserver_api::QUERIES_COUNTED, counts.join(", "));
    }
}

/// One transaction on the controller at `command` and `data`, waiting on it at
/// each step for at most [`EC_STEP`]; `None` once the machine is stopping.
fn transact(claim: &Claim, command: u16, data: u16, mut tx: Transaction) -> Option<u8> {
    let mut waiting: Option<(Wait, Instant)> = None;
    loop {
        let status = claim.port_in(command)?;
        match tx.step(status) {
            Do::Wait(wait) => {
                let since = match waiting {
                    Some((was, since)) if was == wait => since,
                    _ => Instant::now(),
                };
                assert!(since.elapsed() < EC_STEP, "acpiserver: the embedded controller kept {wait:?} unmet for {EC_STEP:?} (EC_SC {status:#04x})");
                waiting = Some((wait, since));
                std::thread::yield_now();
            }
            Do::WriteCommand(byte) => {
                waiting = None;
                claim.port_out(command, byte)?;
            }
            Do::WriteData(byte) => {
                waiting = None;
                claim.port_out(data, byte)?;
            }
            Do::ReadData => {
                waiting = None;
                tx.read(claim.port_in(data)?);
            }
            Do::Done(byte) => return Some(byte),
        }
    }
}
