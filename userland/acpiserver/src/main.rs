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
//! status cleared, then the power button and the embedded controller's GPE
//! are enabled, the controller's backlog is drained, and the SCI
//! acknowledged: a press after the clear latches and is served, one
//! before it is lost.
//!
//! **Then the machine's tables are loaded** ([`aml::load`]), through the
//! kernel's mediated access ([`Claim`]): after the arming, so a press during
//! the load latches and is served when it ends. A table refused, and a DSDT
//! refused, are each said and survived; the power button is served either
//! way. The load hands the kernel `\_S5`'s sleep type, without which the
//! kernel refuses every power-off.
//!
//! **Each SCI** is read off both blocks ([`sci::events`]): a press stops the
//! machine through the supervisor, or is said and dropped on a machine with
//! no power-off, and the controller's GPE drains the
//! controller of every query waiting, which are then run, one by one, as
//! [`aml::query`] says, after the drain that took them. An event this server
//! never enabled, a controller that does not answer, more queries in one
//! drain than [`QUERIES`], and [`sci::EMPTY_SCIS`] SCIs in a row that carried
//! nothing are each a panic naming the registers.
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
mod ec;
mod host;
mod ledger;
mod sci;
mod tables;

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use toyos::endow;
use toyos::ioport::{in16, in8, out16, out8};
use toyos::poller::{Poller, READABLE};
use toyos::power::{self, Stop};
use toyos::AcpiDev;
use toyos_abi::acpi::{Access, AcpiInfo, Block, FIXED_POWER_BUTTON};
use toyos_abi::syscall::{DeviceType, SyscallError};

use ec::{Do, Transaction, Wait};
use aml::Aml;
use battery::Power;
use host::{Answer, Controller, Kernel, Stopping, Take};
use sci::{Event, Served, Unserved, Unstopped, PM1_STATUS, PWRBTN};

/// The most a controller is waited for at one step of a transaction: one that
/// has not moved in this will not.
const EC_STEP: Duration = Duration::from_millis(500);
/// The most queries one drain takes before the controller is a storm.
const QUERIES: usize = 32;
/// How often the log is told the queries' counts, where they moved.
const COUNTS: Duration = Duration::from_secs(30);

struct Server<'a> {
    dev: &'a AcpiDev,
    info: AcpiInfo,
    served: Served,
    /// The controller the row names, if it names one.
    ec: Option<Ports>,
    /// The namespace the load kept, and the power sources found in it.
    aml: Option<Aml<'a, Claim<'a>, Ports>>,
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
    let served = Served {
        power_button: info.flags & FIXED_POWER_BUTTON != 0,
        ec_gpe: info.has_ec().then_some(info.ec_gpe),
    };
    let claim = Claim(&dev);
    let mut server = Server {
        dev: &dev,
        info,
        served,
        ec: info.has_ec().then_some(Ports { command: info.ec_command.port, data: info.ec_data.port }),
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
    let (loaded, kept) = aml::load(&claim, server.ec, server.info.rsdp);
    server.power_off = loaded.handed;
    server.aml = kept;
    if let Some(aml) = &mut server.aml {
        server.power = Power::find(aml, server.ec.is_some());
    }
    server.serve();
}

/// The embedded controller's two ports, which this server's row holds.
#[derive(Clone, Copy)]
struct Ports {
    command: u16,
    data: u16,
}

impl Controller for Ports {
    /// One transaction, waiting on the controller at each step for at most
    /// [`EC_STEP`].
    fn transact(&mut self, mut tx: Transaction) -> u8 {
        let mut waiting: Option<(Wait, Instant)> = None;
        loop {
            let status = in8(self.command);
            match tx.step(status) {
                Do::Wait(wait) => {
                    let since = match waiting {
                        Some((was, since)) if was == wait => since,
                        _ => Instant::now(),
                    };
                    assert!(
                        since.elapsed() < EC_STEP,
                        "acpiserver: the embedded controller kept {wait:?} unmet for {EC_STEP:?} (EC_SC {status:#04x})"
                    );
                    waiting = Some((wait, since));
                    std::thread::yield_now();
                }
                Do::WriteCommand(byte) => {
                    waiting = None;
                    out8(self.command, byte);
                }
                Do::WriteData(byte) => {
                    waiting = None;
                    out8(self.data, byte);
                }
                Do::ReadData => {
                    waiting = None;
                    tx.read(in8(self.data));
                }
                Do::Done(byte) => return byte,
            }
        }
    }
}

/// The claim as the tables' fetch and their AML ask it for what lies outside
/// its own ports.
struct Claim<'a>(&'a AcpiDev);

impl Claim<'_> {
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
        Self::answered("a mediated access", self.0.access(access)).map(|(made, memory_type)| Answer { made, memory_type })
    }

    fn lock_take(&self) -> Result<Take, Stopping> {
        match self.0.lock_take() {
            Err(SyscallError::NotSupported) => Ok(Take::Unusable),
            answer => Self::answered("a take of the Global Lock", answer).map(|taken| if taken { Take::Taken } else { Take::Pending }),
        }
    }

    fn lock_release(&self) -> Result<(), Stopping> {
        Self::answered("the Global Lock's release", self.0.lock_release())
    }

    fn s5(&self, slp_typ_a: u64) -> Result<bool, Stopping> {
        match self.0.s5(slp_typ_a) {
            Err(SyscallError::InvalidArgument) => Ok(false),
            answer => Self::answered("the power-off's sleep type", answer).map(|()| true),
        }
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
        if let Some(n) = self.served.ec_gpe {
            let (_, enable) = bytes(self.info.gpe0).nth(usize::from(n / 8)).expect("the kernel bounded every GPE by the block");
            out8(enable, in8(enable) | 1 << (n % 8));
        }
        if self.info.has_ec() {
            self.drain();
            self.run_queued();
        }
        self.dev.ack().expect("acpiserver: the claim's acknowledgement");
        println!(
            "acpiserver: armed: power button {}, embedded controller {}",
            if self.served.power_button { "served" } else { "not the fixed one, so not served" },
            match self.served.ec_gpe {
                Some(gpe) => format!("on GPE {gpe:#x} at {:#x}/{:#x}", self.info.ec_command.port, self.info.ec_data.port),
                None => "none".into(),
            },
        );
    }

    fn serve(&mut self) -> ! {
        let poller = Poller::new(1);
        let mut next_count = Instant::now() + COUNTS;
        let mut next_read = Instant::now() + battery::POLL;
        // A watch answers once: one still registered from a wait that timed
        // out is not registered again.
        let mut watching = false;
        loop {
            if let (Some(power), Some(aml)) = (&mut self.power, &mut self.aml)
                && Instant::now() >= next_read
            {
                power.read(aml);
                next_read = Instant::now() + battery::POLL;
                if aml.host.stopping {
                    self.power = None;
                }
            }
            if !watching {
                poller.watch(self.dev, READABLE, 0);
                watching = true;
            }
            let until = if self.power.is_some() { next_count.min(next_read) } else { next_count };
            let wait = until.saturating_duration_since(Instant::now());
            poller.wait(1, wait.as_nanos() as u64, |_| watching = false);
            if Instant::now() >= next_count {
                self.log_counts();
                next_count = Instant::now() + COUNTS;
            }
            match self.dev.irq() {
                Ok(record) => self.scis += u64::from(record.count),
                Err(SyscallError::WouldBlock) => continue,
                Err(other) => panic!("acpiserver: the claim's record answered {other:?}"),
            }
            self.take();
            self.dev.ack().expect("acpiserver: the claim's acknowledgement");
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
                    let n = self.info.ec_gpe;
                    let (status, _) = bytes(self.info.gpe0).nth(usize::from(n / 8)).expect("the kernel bounded every GPE by the block");
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
        let mut ec = self.ec.expect("a drain runs only where the row names a controller");
        let mut taken = 0;
        while in8(ec.command) & ec::SCI_EVT != 0 {
            let q = ec.transact(Transaction::query());
            if q == 0 {
                break;
            }
            taken += 1;
            assert!(
                taken <= QUERIES,
                "acpiserver: the embedded controller had more than {QUERIES} queries waiting at once (EC_SC {:#04x})",
                in8(ec.command)
            );
            self.queued.push_back(q);
        }
    }

    fn run_queued(&mut self) {
        while let Some(q) = self.queued.pop_front() {
            aml::query(q);
            let count = self.counts.entry(q).or_insert(0);
            *count += 1;
            self.counted += 1;
            if *count == 1 {
                println!("acpiserver: embedded controller query {q:#04x} taken for the first time, served by nothing: no query's method is evaluated yet");
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
