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
//! status cleared, then the power button, the embedded controller's GPE and
//! [`aml::runtime_gpes`] are enabled, the controller's backlog is drained, and
//! the SCI acknowledged: a press after the clear latches and is served, one
//! before it is lost.
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
mod sci;

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use toyos::endow;
use toyos::ioport::{in16, in8, out16, out8};
use toyos::poller::{Poller, READABLE};
use toyos::power::{self, Stop};
use toyos::AcpiDev;
use toyos_abi::acpi::{AcpiInfo, Block, FIXED_POWER_BUTTON};
use toyos_abi::syscall::{DeviceType, SyscallError};

use ec::{Do, Transaction, Wait};
use sci::{Event, Served, Unserved, PM1_STATUS, PWRBTN};

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
        runtime: aml::runtime_gpes(),
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
    server.serve();
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
        for n in self.served.ec_gpe.into_iter().chain(self.served.runtime.iter().copied()) {
            let (_, enable) = bytes(self.info.gpe0).nth(usize::from(n / 8)).expect("the kernel bounded every GPE by the block");
            out8(enable, in8(enable) | 1 << (n % 8));
        }
        if self.info.has_ec() {
            self.drain();
            self.run_queued();
        }
        self.dev.ack().expect("acpiserver: the claim's acknowledgement");
        println!(
            "acpiserver: armed: power button {}, embedded controller {}, {} GPE(s) the namespace runs",
            if self.served.power_button { "served" } else { "not the fixed one, so not served" },
            match self.served.ec_gpe {
                Some(gpe) => format!("on GPE {gpe:#x} at {:#x}/{:#x}", self.info.ec_command.port, self.info.ec_data.port),
                None => "none".into(),
            },
            self.served.runtime.len(),
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
                    self.clear_gpe(self.info.ec_gpe);
                    self.drain();
                }
                Event::Runtime(n) => {
                    let gpe = aml::gpe(n);
                    if gpe.trigger == aml::Trigger::Edge {
                        self.clear_gpe(n);
                    }
                    match gpe.disposition {
                        aml::Disposition::Unserved => {}
                    }
                    if gpe.trigger == aml::Trigger::Level {
                        self.clear_gpe(n);
                    }
                }
            }
        }
        self.run_queued();
    }

    fn clear_gpe(&self, n: u16) {
        let (status, _) = bytes(self.info.gpe0).nth(usize::from(n / 8)).expect("the kernel bounded every GPE by the block");
        out8(status, 1 << (n % 8));
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
            match aml::query(q) {
                aml::Disposition::Unserved => {}
            }
            let count = self.counts.entry(q).or_insert(0);
            *count += 1;
            self.counted += 1;
            if *count == 1 {
                println!("acpiserver: embedded controller query {q:#04x} taken for the first time, served by nothing: stage 1 runs no AML");
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
