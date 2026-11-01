//! The VM's console: this boot's log, from its first line, written to the
//! port named [`toyos_virtio_serial::CONSOLE_PORT`], which QEMU puts on its
//! stdio.
//!
//! **What it writes is `logkeeper`'s**: the driver asks to be the console's
//! carrier ([`toyos_logstream::CARRIER`]) and says how much of the log the
//! host has taken, and `logkeeper` quiets the console UART once the host has
//! taken the first bytes and gives it back when this program goes.
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

use toyos::poller::{Poller, READABLE};
use toyos::say;
use toyos::Pipe;
use toyos_abi::syscall::SyscallError;
use toyos_virtio_serial::control::{Control, Event};
use toyos_virtio_serial::probe::{Probe, Step};
use toyos_virtio_serial::PORT_ID;

use crate::channel::Channel;

/// How long the host may take nothing before that is said.
const SILENT: Duration = Duration::from_secs(10);

const TOKEN_CLAIM: u64 = 0;
const TOKEN_LOG: u64 = 1;

/// The port, once it is the console's, and the log it carries.
struct Console {
    channel: Channel,
    probe: Probe,
    log: Pipe,
    /// Bytes of the log the host has taken.
    taken: u64,
    /// The most recent count `logkeeper` was told.
    told: u64,
    /// Since when the host has taken nothing of a transmit, once that is said.
    silent_since: Option<Instant>,
}

pub fn run(mut channel: Channel, probe: Probe, held: Vec<Control>, poller: Poller) -> ! {
    channel.send_control(Control { id: PORT_ID, event: Event::PortOpen, value: 1 });
    let (conn, served) = logkeeper_api::carry().unwrap_or_else(|why| panic!("virtio-console: logkeeper: {why}"));
    say!("virtio-console: carrying the log on port {PORT_ID}, from its first line");
    let mut console = Console { channel, probe, log: served.pipe, taken: 0, told: 0, silent_since: None };
    for control in held {
        console.control(control);
    }
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
        // Once the silence is said, only the device's taking the transmit,
        // which interrupts, has anything to change.
        let timeout = match console.silent_since {
            Some(_) => u64::MAX,
            None => console.channel.oldest().map_or(u64::MAX, |at| crate::until(at, SILENT)),
        };
        let mut interrupted = false;
        poller.wait(1, timeout, |token| interrupted |= token == TOKEN_CLAIM);
        if interrupted {
            console.channel.take_interrupt();
        }
    }
}

impl Console {
    /// Every control message the device sent since the last round.
    fn controls(&mut self) {
        while let Some(received) = self.channel.control() {
            match self.probe.take(received.bytes()) {
                Ok(Step::Reply(reply)) => self.channel.send_control(reply),
                Ok(Step::Driver(control)) => self.control(control),
                Ok(Step::Nothing) => {}
                Err(why) => panic!("virtio-console: {why}"),
            }
        }
    }

    /// A message about the console's port.
    fn control(&mut self, Control { event, value, .. }: Control) {
        match event {
            Event::DeviceRemove => panic!("virtio-console: the device removed the console's port {PORT_ID}"),
            Event::PortOpen => {
                say!("virtio-console: the host {} the console's port", if value != 0 { "opened" } else { "closed" });
            }
            _ => {}
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
