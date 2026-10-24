//! `/system/bin/virtio-console` — the VM's console: this boot's log, from its
//! first line, written to the virtio-serial port named
//! [`CONSOLE_PORT`], which QEMU puts on its stdio.
//!
//! **One process per virtio-serial function the kernel does not drive**, as
//! `vdagent`'s row has: a function none of whose ports carries the console's
//! name is another driver's, and this program ends with exit 0, which hands
//! the function to the next row that drives one ([`Probe`]).
//!
//! **What it writes is `logkeeper`'s**: it asks to be the console's carrier
//! ([`toyos_logstream::CARRIER`]) and says how much of the log the host has
//! taken, and `logkeeper` quiets the console UART once the host has taken the
//! first bytes and gives it back when this program goes.
//!
//! **The host's pace is the console's**: a `virtserialport` holds a transmit
//! until its chardev takes it, so a transmit is made only while the device has
//! a free buffer, and the log waits in `logkeeper` meanwhile. A host that
//! takes nothing for [`SILENT`] is said once, and never ends the program: a
//! slow host is not a failed device.
//!
//! **The port posts no receive buffer**, so what the host types stays with
//! the host.

use std::time::{Duration, Instant};

use toyos::endow;
use toyos::poller::{Poller, READABLE};
use toyos::say;
use toyos::{PciDev, Pipe};
use toyos_abi::syscall::{PciId, SyscallError};
use toyos_manifest::device::PciMatch;
use toyos_virtio_serial::control::{Control, Event};
use toyos_virtio_serial::probe::{Probe, Step, Verdict};
use toyos_virtio_serial::{CONSOLE_PORT, PORT_ID};
use toyos_virtio_serial_claim::{Channel, Shape};

/// A communication controller of class `07:80:00`, which virtio 1.2 §5.3.1
/// numbers `0x1040 + 3`: the matcher its row drives, and the label its claim
/// is endowed under.
const VIRTIO_SERIAL: PciMatch = PciMatch::new([0x07, 0x80, 0x00], Some(PciId { vendor: 0x1af4, device: 0x1043 }));

/// Four transmits in flight, so the host is handed the next while it writes
/// one out, of a size that makes each a few hundred lines.
const SHAPE: Shape = Shape { receive: None, transmit: (4, 32 * 1024) };

/// How long the device may take to name its ports. Policy, far past what QEMU
/// 11.1.1 takes: it adds and names them at the driver's first messages.
const PROBE_BOUND: Duration = Duration::from_secs(5);

/// How long the host may take nothing before that is said.
const SILENT: Duration = Duration::from_secs(10);

const TOKEN_CLAIM: u64 = 0;
const TOKEN_LOG: u64 = 1;

fn main() {
    let claim: PciDev = endow::claim(&VIRTIO_SERIAL.to_string())
        .expect("virtio-console: its row drives a function, and it is started holding the function's claim");
    let mut channel = Channel::open(claim, SHAPE, "virtio-console")
        .unwrap_or_else(|why| panic!("virtio-console: the function is not driven: {why}"));
    let poller = Poller::new(2);
    let mut probe = Probe::new(CONSOLE_PORT);
    identify(&mut channel, &mut probe, &poller);
    channel.send_control(Control { id: PORT_ID, event: Event::PortOpen, value: 1 });

    let (conn, served) = logkeeper_api::carry().unwrap_or_else(|why| panic!("virtio-console: logkeeper: {why}"));
    say!("virtio-console: carrying the log on port {PORT_ID}, from its first line");
    let mut console = Console { channel, probe, log: served.pipe, taken: 0, told: 0, silent_since: None };
    loop {
        console.controls();
        let taken = console.channel.transmitted();
        console.taken += taken;
        if console.taken > console.told {
            logkeeper_api::delivered(&conn, console.taken);
            console.told = console.taken;
        }
        console.transmit();
        console.say_silence(taken > 0);

        poller.watch(console.channel.claim(), READABLE, TOKEN_CLAIM);
        if console.channel.can_transmit() {
            poller.watch(&console.log, READABLE, TOKEN_LOG);
        }
        let timeout = console.channel.oldest().map_or(u64::MAX, |at| {
            let left = (at + SILENT).saturating_duration_since(Instant::now());
            u64::try_from(left.as_nanos()).unwrap_or(u64::MAX)
        });
        let mut interrupted = false;
        poller.wait(1, timeout, |token| interrupted |= token == TOKEN_CLAIM);
        if interrupted {
            console.channel.take_interrupt();
        }
    }
}

/// Read the device's control messages until a port carries the console's
/// name, or end with exit 0 where none does: the function is another
/// driver's.
fn identify(channel: &mut Channel, probe: &mut Probe<'_>, poller: &Poller) {
    let deadline = Instant::now() + PROBE_BOUND;
    loop {
        while let Some(received) = channel.control() {
            match probe.take(received.bytes()) {
                Ok(Step::Reply(reply)) => channel.send_control(reply),
                Ok(Step::Driver(_) | Step::Nothing) => {}
                Err(why) => panic!("virtio-console: {why}"),
            }
        }
        match probe.verdict() {
            Verdict::Ours => return,
            Verdict::Foreign(names) => {
                say!("virtio-console: the function's ports are {names:?}, none {CONSOLE_PORT}; another row drives it");
                std::process::exit(0);
            }
            Verdict::Probing => {}
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            say!("virtio-console: the function named no port {CONSOLE_PORT} in {PROBE_BOUND:?}; another row drives it");
            std::process::exit(0);
        }
        poller.watch(channel.claim(), READABLE, TOKEN_CLAIM);
        poller.wait(1, u64::try_from(left.as_nanos()).unwrap_or(u64::MAX), |_| {});
        channel.take_interrupt();
    }
}

/// The port, once it is the console's, and the log it carries.
struct Console<'p> {
    channel: Channel,
    probe: Probe<'p>,
    log: Pipe,
    /// Bytes of the log the host has taken.
    taken: u64,
    /// The most recent count `logkeeper` was told.
    told: u64,
    /// Since when the host has taken nothing of a transmit, once that is said.
    silent_since: Option<Instant>,
}

impl Console<'_> {
    /// Every control message the device sent since the port was the console's.
    fn controls(&mut self) {
        while let Some(received) = self.channel.control() {
            match self.probe.take(received.bytes()) {
                Ok(Step::Reply(reply)) => self.channel.send_control(reply),
                Ok(Step::Driver(Control { id, event: Event::DeviceRemove, .. })) if self.probe.is_ours(id) => {
                    panic!("virtio-console: the device removed the console's port {id}")
                }
                Ok(Step::Driver(Control { id, event: Event::PortOpen, value })) if self.probe.is_ours(id) => {
                    say!("virtio-console: the host {} the console's port", if value != 0 { "opened" } else { "closed" });
                }
                Ok(Step::Driver(_) | Step::Nothing) => {}
                Err(why) => panic!("virtio-console: {why}"),
            }
        }
    }

    /// As much of the log as the device has free buffers for, read straight
    /// into them.
    fn transmit(&mut self) {
        while self.channel.can_transmit() {
            let log = &self.log;
            let mut ended = false;
            let sent = self.channel.transmit(|buffer| match log.read_nonblock(buffer) {
                Ok(0) => {
                    ended = true;
                    0
                }
                Ok(n) => n,
                Err(SyscallError::WouldBlock) => 0,
                Err(why) => panic!("virtio-console: logkeeper's pipe refused a read: {why:?}"),
            });
            if ended {
                panic!("virtio-console: logkeeper closed the log's pipe");
            }
            if sent == 0 {
                return;
            }
        }
    }

    /// Say once that the host has taken nothing of a transmit for [`SILENT`],
    /// and once that it takes the console again.
    fn say_silence(&mut self, took: bool) {
        match (self.silent_since, took) {
            (Some(since), true) => {
                say!("virtio-console: the host takes the console again after {} s", since.elapsed().as_secs());
                self.silent_since = None;
            }
            (None, false) => {
                if let Some(at) = self.channel.oldest().filter(|at| at.elapsed() >= SILENT) {
                    say!("virtio-console: the host has taken nothing for {} s; the console waits on it", SILENT.as_secs());
                    self.silent_since = Some(at);
                }
            }
            _ => {}
        }
    }
}
