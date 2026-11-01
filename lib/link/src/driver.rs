//! A NIC driver's side of a link: the session with the stack, carried over the
//! device one [`Nic`] drives.
//!
//! **One pass per wake**: the device's interrupt and link, then its received
//! frames into the receive ring while it has room, then the transmit ring's
//! frames into the device while it has room, then the stack's groups and its
//! ask for counters, then one doorbell if anything changed. A pass takes at
//! most a ring of frames each way, so a peer that keeps one full cannot keep
//! the other waiting.
//!
//! **A stack that hangs up is connected to again at once**, with a fresh
//! region, and the device stays up meanwhile. The stack hangs up on a link
//! whose driver wrote what no driver writes, and refuses that address's next
//! hello [`Refusal::Faulted`], on which this driver ends: its supervisor's
//! restart budget is what bounds a driver that keeps doing it. Any other
//! refusal, or a port closed for good, ends [`drive`]: nothing this driver
//! does changes either.

use toyos::endow::{self, EndowError};
use toyos::ipc::{Connection, FrameRx, RxStep};
use toyos::wake::{Bell, Waker};
use toyos::poller::{Poller, READABLE};
use toyos::shm::ReadBytes;
use toyos::{say, PciDev};

use crate::ends::{Driver, Fault};
use crate::layout::{RX_DEPTH, SLOT_BYTES, TX_DEPTH};
use crate::region::Region;
use crate::state::{Counters, Group, Link};
use crate::wire::{self, Address, Hello, Medium, Refusal};
use crate::PORT;

/// The device under a link, as its driver drives it.
pub trait Nic {
    /// What the device's link is carried over.
    const MEDIUM: Medium;

    fn mac(&self) -> [u8; 6];

    /// The claim, for the poller: readable means an interrupt has landed.
    fn claim(&self) -> &PciDev;

    /// Take the interrupt and what the device owes since the last pass, and
    /// answer the link as it is now.
    fn begin_pass(&mut self) -> Link;

    /// Hand the next received frame to `take` and give its buffer back;
    /// `false` when none waits.
    fn receive(&mut self, take: &mut dyn FnMut(&[u8])) -> bool;

    /// Every buffer a pass's receives gave back reaches the device.
    fn received(&mut self) {}

    /// How many frames the device takes now.
    fn tx_room(&mut self) -> usize;

    /// Have the claim read ready when room returns, and answer the room there
    /// is once that is so.
    fn wake_on_room(&mut self) -> usize {
        self.tx_room()
    }

    /// Copy `frame` into the device and send it, for a caller
    /// [`Self::tx_room`] answered room.
    fn send(&mut self, frame: ReadBytes<'_>);

    /// How long until a pass is owed with no interrupt, in nanoseconds.
    fn pass_due_in(&self) -> Option<u64> {
        None
    }

    /// Pass frames sent to `group`.
    fn join(&mut self, group: Group);

    /// What the driver and the device counted so far.
    fn counters(&mut self) -> Counters {
        Counters::default()
    }

    /// Say what changed of what the driver counts, once a pass.
    fn report(&mut self) {}
}

const TOKEN_CLAIM: u64 = 0;
const TOKEN_LINK: u64 = 1;

/// Drive `nic` for the stack under `label`, its driver's name, until the
/// stack refuses it or its port closes for good.
///
/// # Panics
/// On anything the stack writes that no stack of the protocol writes, on the
/// stack's refusal for what this driver wrote, on a device whose address is a
/// group's, and on a manifest that gives this driver no link: nothing drives
/// on from any of them.
pub fn drive(label: &str, nic: &mut impl Nic) {
    loop {
        let conn = match endow::service(PORT) {
            Ok(conn) => conn,
            Err(EndowError::ServerGone) => {
                say!("{label}: the stack's port is closed for good; this device carries nothing");
                return;
            }
            Err(why) => panic!("{label}: the manifest gives this driver `{PORT}`, and it answered {why:?}"),
        };
        let mut session = Session::open(label, nic, conn);
        match session.serve(nic) {
            Ended::Refused(Refusal::Faulted) => panic!("{label}: the stack ended this driver's link for what it wrote"),
            Ended::Refused(why) => {
                say!("{label}: the stack refused this link: {why:?}");
                return;
            }
            Ended::HungUp(why) => say!("{label}: the stack's connection ended ({why}); connecting again"),
        }
    }
}

enum Ended {
    Refused(Refusal),
    HungUp(&'static str),
}

struct Session<'a> {
    label: &'a str,
    /// The hello's connection, until the stack answers it.
    conn: Option<Connection>,
    region: Region,
    end: Driver,
    rx: FrameRx<4>,
    /// Once the link is open, the stack's two bells: the waker rung for the
    /// stack, and the bell it rings, its waker going being the stack's end.
    bells: Option<(Waker, Bell)>,
    said: Option<Link>,
    /// Received frames longer than a slot, dropped.
    oversize: u64,
    oversize_said: u64,
}

impl<'a> Session<'a> {
    /// A fresh region and its end, said hello with over `conn`.
    fn open<N: Nic>(label: &'a str, nic: &N, conn: Connection) -> Self {
        let region = Region::create().unwrap_or_else(|why| panic!("{label}: no region for a link: {why:?}"));
        let end = Driver::new(region.words());
        let mac = Address::new(nic.mac())
            .unwrap_or_else(|| panic!("{label}: the device's address is a group's, which no link carries"));
        let hello = Hello { mac, medium: N::MEDIUM, label: label.into() };
        let shared = region.share().unwrap_or_else(|why| panic!("{label}: the link's region would not share: {why:?}"));
        // A connection that will not take the hello is one whose stack has
        // gone: its hang-up is read in the first pass.
        let _ = conn.try_send_bytes_with(wire::MSG_HELLO, &hello.encode(), [shared]);
        Self {
            label,
            conn: Some(conn),
            region,
            end,
            rx: FrameRx::new(),
            bells: None,
            said: None,
            oversize: 0,
            oversize_said: 0,
        }
    }

    fn serve(&mut self, nic: &mut impl Nic) -> Ended {
        let poller = Poller::new(2);
        loop {
            let ring = self.pass(nic).unwrap_or_else(|why| panic!("{}: the stack wrote {why}", self.label));
            if let Some((waker, _)) = &self.bells
                && ring
                && waker.ring().is_err()
            {
                return Ended::HungUp("its doorbell is gone");
            }
            poller.watch(nic.claim(), READABLE, TOKEN_CLAIM);
            match (&self.bells, &self.conn) {
                (Some((_, rung)), _) => poller.watch(rung, READABLE, TOKEN_LINK),
                (None, Some(conn)) => poller.watch(conn, READABLE, TOKEN_LINK),
                (None, None) => unreachable!("{}: a link neither opening nor open", self.label),
            }
            poller.wait(1, nic.pass_due_in().unwrap_or(u64::MAX), |_| {});
            if let Some(ended) = self.hear() {
                return ended;
            }
        }
    }

    /// One pass; whether the stack is to be rung.
    fn pass(&mut self, nic: &mut impl Nic) -> Result<bool, Fault> {
        let page = self.region.words();
        let bytes = self.region.bytes();
        let mut ring = false;

        let link = nic.begin_pass();
        if self.said != Some(link) {
            self.end.set_link(page, link);
            self.said = Some(link);
            ring = true;
        }

        let mut room = self.end.room(page)?;
        for _ in 0..RX_DEPTH {
            if room == 0 {
                room = self.end.want_room(page)?;
                if room == 0 {
                    break;
                }
            }
            let slot = self.end.slot();
            let mut len = 0;
            let mut oversize = false;
            let took = nic.receive(&mut |frame| match frame.len() {
                0 => {}
                n if n > SLOT_BYTES => oversize = true,
                n => {
                    bytes.write(slot.offset, frame);
                    len = n;
                }
            });
            if !took {
                break;
            }
            self.oversize += u64::from(oversize);
            if len > 0 {
                self.end.receive(page, len)?;
                room -= 1;
            }
        }
        nic.received();
        ring |= self.end.publish(page);

        for _ in 0..TX_DEPTH {
            if nic.tx_room() == 0 && nic.wake_on_room() == 0 {
                break;
            }
            let Some(frame) = self.end.to_send(page)? else { break };
            nic.send(*bytes.sub(frame.offset, frame.len));
        }
        ring |= self.end.release(page);

        while let Some(group) = self.end.joined(page)? {
            nic.join(group);
        }
        if let Some(asked) = self.end.counters_asked(page) {
            self.end.answer(page, asked, &nic.counters());
            ring = true;
        }
        nic.report();
        if self.oversize != self.oversize_said {
            say!("{}: dropped {} received frame(s) longer than a link's {SLOT_BYTES}-byte slot", self.label, self.oversize);
            self.oversize_said = self.oversize;
        }
        Ok(ring)
    }

    /// Read the stack's answer, then its doorbell; the end of the session
    /// if that is what came.
    fn hear(&mut self) -> Option<Ended> {
        if let Some((_, rung)) = &self.bells {
            return rung.try_take().err().map(|_| Ended::HungUp("its process ended"));
        }
        let conn = self.conn.as_ref().expect("a link not open is opening");
        match self.rx.pump(conn) {
            RxStep::Idle => None,
            RxStep::Eof => Some(Ended::HungUp("its process ended before it answered")),
            RxStep::Frame { msg_type: wire::MSG_OPENED, payload_len: 0, handles: wire::OPENED_HANDLES } => {
                let [ring, rung] = self.rx.take_handles::<{ wire::OPENED_HANDLES }>().expect("counted above");
                self.bells = Some((Waker::from(ring), Bell::from(rung)));
                // Its work is done: the bells carry the link from here.
                self.conn = None;
                say!("{}: the link is open", self.label);
                self.hear()
            }
            RxStep::Frame { msg_type: wire::MSG_REFUSED, payload_len, handles: 0 } => {
                match Refusal::decode(self.rx.payload(payload_len)) {
                    Some(why) => Some(Ended::Refused(why)),
                    None => panic!("{}: the stack refused this link with a word no stack writes", self.label),
                }
            }
            RxStep::Frame { .. } | RxStep::Malformed => panic!("{}: the stack answered a hello with a frame no stack writes", self.label),
        }
    }
}
