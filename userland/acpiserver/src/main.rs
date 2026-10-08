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
//! way.
//!
//! **Each SCI** is read off both blocks ([`sci::events`]): a press stops the
//! machine through the supervisor, and the controller's GPE drains the
//! controller of every query waiting, which are then run, one by one, as
//! [`aml::query`] says, after the drain that took them. An event this server
//! never enabled, a controller that does not answer, more queries in one
//! drain than [`QUERIES`], and [`sci::EMPTY_SCIS`] SCIs in a row that carried
//! nothing are each a panic naming the registers.
//!
//! The log carries each query number the first time it is taken and a count
//! of every one at [`COUNTS`] intervals, never a line per event.

mod aml;
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
use toyos_abi::acpi::{Access, AcpiInfo, Block, FIXED_POWER_BUTTON};
use toyos_abi::syscall::{DeviceType, SyscallError};

use ec::{Do, Transaction, Wait};
use host::{Answer, Kernel, Stopping, Take};
use sci::{Event, Served, Unserved, GBL, PM1_STATUS, PWRBTN};

/// The most a controller is waited for at one step of a transaction: one that
/// has not moved in this will not.
const EC_STEP: Duration = Duration::from_millis(500);
/// The most queries one drain takes before the controller is a storm.
const QUERIES: usize = 32;
/// How often the log is told the queries' counts, where they moved.
const COUNTS: Duration = Duration::from_secs(30);

struct Server {
    dev: AcpiDev,
    info: AcpiInfo,
    served: Served,
    /// Taken off the controller by the drain, run after it.
    queued: VecDeque<u8>,
    /// Every query number taken, and how often.
    counts: BTreeMap<u8, u64>,
    counted: u64,
    logged: u64,
    scis: u64,
    empty: u32,
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
    let mut server = Server {
        dev,
        info,
        served,
        queued: VecDeque::new(),
        counts: BTreeMap::new(),
        counted: 0,
        logged: 0,
        scis: 0,
        empty: 0,
    };
    server.arm();
    let waited = {
        let claim = Claim { dev: &server.dev, info: server.info, scis: Cell::new(0) };
        aml::load(&claim, server.info.rsdp);
        claim.scis.get()
    };
    server.scis += waited;
    server.serve();
}

/// The claim as the tables' fetch and their AML ask it for what lies outside
/// its own ports.
struct Claim<'a> {
    dev: &'a AcpiDev,
    info: AcpiInfo,
    /// SCIs taken while a take of the Global Lock waited for the firmware.
    scis: Cell<u64>,
}

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

    /// Take the SCI's record where there is one, and have the line unmasked.
    fn unmask(&self) {
        match self.dev.irq() {
            Ok(record) => self.scis.set(self.scis.get() + u64::from(record.count)),
            Err(SyscallError::WouldBlock) => {}
            Err(other) => panic!("acpiserver: the claim's record answered {other:?}"),
        }
        self.dev.ack().expect("acpiserver: the claim's acknowledgement");
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

    /// The firmware says it let the lock go by raising the SCI with
    /// `GBL_STS` (ACPI 6.5 §5.2.10.1). While this waits, that is the only
    /// event enabled: every other one stays latched in its status bit and
    /// raises the line again once its enable is back, so the SCI this takes
    /// is the firmware's word or nothing.
    fn released(&self, within: Duration) -> Option<Duration> {
        let began = Instant::now();
        let pm1 = self.info.pm1_event;
        let pm1_enabled = in16(pm1.enable());
        let gpe_enabled: Vec<(u16, u8)> = bytes(self.info.gpe0).map(|(_, enable)| (enable, in8(enable))).collect();
        for &(enable, _) in &gpe_enabled {
            out8(enable, 0);
        }
        out16(pm1.enable(), GBL);
        let poller = Poller::new(1);
        let mut watching = false;
        let signalled = loop {
            if in16(pm1.port) & GBL != 0 {
                out16(pm1.port, GBL);
                break true;
            }
            let left = within.saturating_sub(began.elapsed());
            if left.is_zero() {
                break false;
            }
            self.unmask();
            if !watching {
                poller.watch(self.dev, READABLE, 0);
                watching = true;
            }
            poller.wait(1, left.as_nanos() as u64, |_| watching = false);
        };
        // The line as the serving loop expects it: unmasked, and no record
        // of an SCI this wait has already answered.
        self.unmask();
        out16(pm1.enable(), pm1_enabled);
        for (enable, was) in gpe_enabled {
            out8(enable, was);
        }
        signalled.then(|| began.elapsed())
    }
}

/// Each byte of a status-and-enable block: its status port and its enable port.
fn bytes(block: Block) -> impl Iterator<Item = (u16, u16)> {
    (0..block.len / 2).map(move |i| (block.port + i, block.enable() + i))
}

impl Server {
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
        // A watch answers once: one still registered from a wait that timed
        // out is not registered again.
        let mut watching = false;
        loop {
            if !watching {
                poller.watch(&self.dev, READABLE, 0);
                watching = true;
            }
            let wait = next_count.saturating_duration_since(Instant::now());
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

    fn press(&mut self) -> ! {
        println!("acpiserver: the power button was pressed, on SCI {} of this boot; asking the supervisor to power off", self.scis);
        let refused = power::stop(Stop::Shutdown);
        panic!("acpiserver: the power-off was refused: {refused:?}");
    }

    /// Take every query the controller has waiting off it, queued for after.
    fn drain(&mut self) {
        let mut taken = 0;
        while in8(self.info.ec_command.port) & ec::SCI_EVT != 0 {
            let q = self.transact(Transaction::query());
            if q == 0 {
                break;
            }
            taken += 1;
            assert!(
                taken <= QUERIES,
                "acpiserver: the embedded controller had more than {QUERIES} queries waiting at once (EC_SC {:#04x})",
                in8(self.info.ec_command.port)
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

    /// One transaction, waiting on the controller at each step for at most
    /// [`EC_STEP`].
    fn transact(&self, mut tx: Transaction) -> u8 {
        let (command, data) = (self.info.ec_command.port, self.info.ec_data.port);
        let mut waiting: Option<(Wait, Instant)> = None;
        loop {
            let status = in8(command);
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
                    out8(command, byte);
                }
                Do::ReadData => {
                    waiting = None;
                    tx.read(in8(data));
                }
                Do::Done(byte) => return byte,
            }
        }
    }

    fn log_counts(&mut self) {
        if self.counted == self.logged {
            return;
        }
        self.logged = self.counted;
        let counts: Vec<String> = self.counts.iter().map(|(q, n)| format!("{q:#04x} x{n}")).collect();
        println!("acpiserver: {} SCIs; embedded controller queries taken: {}", self.scis, counts.join(", "));
    }
}
