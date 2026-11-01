//! The stack's side of a link: the region a driver sent with its hello, the
//! stack's end of the region's rings (`toyos-link`) and the two doorbells,
//! both of which the stack makes, so that it holds no event a driver sent.
//!
//! **What the driver writes is believed only as `toyos-link`'s ends bound
//! it**: a frame's length by its slot, the link by its word, and the counters
//! not at all, displayed as the driver wrote them. A [`Fault`] ends the link,
//! as a hang-up does, and the stack keeps every connection it carries.

use toyos::ipc::{Connection, FrameRx, RxStep};
use toyos::poller::READABLE;
use toyos::say;
use toyos::wake::{Bell, Waker};
use toyos_inspect::Snapshot;
use toyos_link::ends::{Fault, Stack};
use toyos_link::layout::{RX_DEPTH, SLOT_BYTES};
use toyos_link::region::Region;
use toyos_link::state::{Counters, Group, Link};
use toyos_link::wire::{self, Hello, Refusal};
use toyos_net_node::{Credit, IfIndex, Node};
use toyos_net_wire::ethernet::{IndividualMac, MacAddr};
use toyos_net_wire::Instant;

use crate::watches::Watches;

const _: () = assert!(toyos_net_ip::FRAME <= SLOT_BYTES, "every frame the stack sends fits a link's slot");

/// A driver's connection that has not said hello yet.
pub struct Pending {
    conn: Connection,
    rx: FrameRx<{ Hello::MAX_BYTES }>,
    pub since: std::time::Instant,
}

/// What a pending connection's hello came to.
pub enum Heard {
    Nothing,
    /// The driver went, or said what no driver says: it was refused, or let go.
    Gone,
    /// The driver said who it is, its address as the node takes one, with
    /// its region: its connection is [`Pending::into_conn`].
    Hello(Hello, IndividualMac, Region),
}

impl Pending {
    pub fn new(conn: Connection) -> Self {
        Self { conn, rx: FrameRx::new(), since: std::time::Instant::now() }
    }

    pub fn watch(&self, poller: &mut Watches, token: u64) {
        poller.watch(&self.conn, READABLE, token);
    }

    pub fn into_conn(self) -> Connection {
        self.conn
    }

    pub fn hear(&mut self) -> Heard {
        match self.rx.pump(&self.conn) {
            RxStep::Idle => Heard::Nothing,
            RxStep::Eof => Heard::Gone,
            RxStep::Frame { msg_type: wire::MSG_HELLO, payload_len, handles: 1 } => {
                let hello = Hello::decode(self.rx.payload(payload_len));
                let mac = hello.as_ref().and_then(|hello| IndividualMac::new(MacAddr(hello.mac.bytes())));
                let [region] = self.rx.take_handles::<1>().expect("counted above");
                match (hello, mac, Region::lent(region).ok()) {
                    (Some(hello), Some(mac), Some(region)) => Heard::Hello(hello, mac, region),
                    _ => {
                        say!("netstack: a NIC driver's hello is not one this protocol makes, or its region is none");
                        refuse(&self.conn, Refusal::Malformed);
                        Heard::Gone
                    }
                }
            }
            RxStep::Frame { .. } | RxStep::Malformed => {
                say!("netstack: a NIC driver's connection sent a frame no driver sends first");
                Heard::Gone
            }
        }
    }
}

/// Answer a driver's hello with `why`; one `try_send`, as every answer.
pub fn refuse(conn: &Connection, why: Refusal) {
    let _ = conn.try_send_bytes(wire::MSG_REFUSED, &why.encode());
}

/// Why a link ended.
pub enum Ended {
    HungUp,
    Fault(Fault),
}

/// A link the stack opened.
pub struct Session {
    /// Rung for the driver.
    ring_driver: Waker,
    /// The driver's doorbell; its waker going is the driver's end.
    rung: Bell,
    region: Region,
    end: Stack,
    hello: Hello,
    /// The node's interface for the link.
    iface: IfIndex,
    /// Frames the stack handed the link: its own count, not the driver's.
    sent: u64,
    /// The link as the node was last told it.
    said: Link,
    frame: Box<[u8; SLOT_BYTES]>,
    /// The driver owes an answer to the counters asked.
    asked: bool,
    ring: bool,
}

impl Session {
    /// Open the link `hello` asked for over `region` as the node's `iface`,
    /// and say so to the driver with its ends of the two bells, its `conn`
    /// let go then; `None` for a driver gone before it heard.
    pub fn open(hello: Hello, iface: IfIndex, conn: Connection, region: Region, groups: &[Group]) -> Option<Self> {
        let mut end = Stack::new(region.words());
        for group in groups {
            assert!(end.join(region.words(), *group), "netstack: more groups than a link holds");
        }
        let (Ok((ring_driver, driver_hears)), Ok((driver_rings, rung))) = (toyos::wake::pair(), toyos::wake::pair()) else {
            say!("netstack: no doorbells for {}'s link", hello.label);
            return None;
        };
        conn.try_signal_with(wire::MSG_OPENED, [driver_rings.into(), driver_hears.into()]).ok()?;
        Some(Self {
            ring_driver,
            rung,
            region,
            end,
            hello,
            iface,
            sent: 0,
            said: Link::Down,
            frame: Box::new([0; SLOT_BYTES]),
            asked: false,
            ring: true,
        })
    }

    pub fn iface(&self) -> IfIndex {
        self.iface
    }

    /// The link's end if its driver has hung up, read now.
    pub fn alive(&self) -> Result<(), Ended> {
        self.hear()
    }

    /// Whether the node was last told this link is up.
    pub fn is_up(&self) -> bool {
        self.said.is_up()
    }

    pub fn watch(&self, poller: &mut Watches, token: u64) {
        poller.watch(&self.rung, READABLE, token);
    }

    /// The link's state, the frames the driver received, and what is due
    /// to the node; `true` when the receive stopped at its bound, and another
    /// pass is owed at once.
    pub fn receive(&mut self, node: &mut Node, now: Instant, draw: fn() -> u32) -> Result<bool, Ended> {
        self.hear()?;
        let link = self.end.link(self.region.words()).map_err(Ended::Fault)?;
        if link.is_up() != self.said.is_up() {
            node.link(now, self.iface, link.is_up(), draw);
        }
        self.said = link;
        let (words, bytes, end, frame) = (self.region.words(), self.region.bytes(), &mut self.end, &mut self.frame);
        let mut fault = None;
        let owed = node.receive(
            now,
            self.iface,
            RX_DEPTH as usize,
            |sink| match end.received(words) {
                Ok(Some(span)) => {
                    let frame = &mut frame[..span.len];
                    bytes.read(span.offset, frame);
                    sink(frame);
                    true
                }
                Ok(None) => false,
                Err(why) => {
                    fault = Some(why);
                    false
                }
            },
            draw,
        );
        self.ring |= self.end.release(words);
        match fault {
            Some(why) => Err(Ended::Fault(why)),
            None => Ok(owed),
        }
    }

    /// The transmit ring's room, armed to be rung for when there is none.
    fn room(&mut self) -> Result<usize, Fault> {
        let words = self.region.words();
        Ok(match self.end.room(words)? {
            0 => self.end.want_room(words)?,
            room => room,
        } as usize)
    }

    /// One frame into the transmit ring's next slot.
    fn send(&mut self, frame: &[u8]) -> Result<(), Fault> {
        self.region.bytes().write(self.end.slot().offset, frame);
        self.end.send(self.region.words(), frame.len())?;
        self.sent = self.sent.saturating_add(1);
        Ok(())
    }

    /// What was sent reaches the driver, which is rung for it.
    fn publish(&mut self) {
        self.ring |= self.end.publish(self.region.words());
    }

    /// Ask the driver for its counters, which [`Self::counters`] reads once
    /// it answered.
    pub fn ask_counters(&mut self) {
        if !self.asked {
            self.end.ask_counters(self.region.words());
            self.asked = true;
            self.ring = true;
        }
    }

    /// The counters asked for, once the driver answered; `None` while it owes
    /// them.
    pub fn counters(&mut self) -> Option<Counters> {
        if !self.asked {
            return None;
        }
        let counters = self.end.counters(self.region.words())?;
        self.asked = false;
        Some(counters)
    }

    /// Ring the driver if this pass gave it anything to do.
    pub fn ring(&mut self) -> Result<(), Ended> {
        if std::mem::take(&mut self.ring) {
            self.ring_driver.ring().map_err(|_| Ended::HungUp)?;
        }
        Ok(())
    }

    /// Take the driver's doorbell; its waker gone is the link's end.
    fn hear(&self) -> Result<(), Ended> {
        self.rung.try_take().map(drop).map_err(|_| Ended::HungUp)
    }

    /// What `inspect` reads about the link: its driver, its address, its
    /// state, and what the driver counted.
    ///
    /// **An unreported link is `unreported`, not `up`**: the device tells the
    /// stack nothing about one, and the stack serving as though it were up is
    /// its assumption rather than something it measured.
    pub fn inspect(&self, at: &str, counters: Option<&Counters>, snap: &mut Snapshot) {
        let m = self.hello.mac.bytes();
        snap.put(&format!("{at}.mac"), format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5]));
        snap.put(&format!("{at}.driver"), self.hello.label.as_str());
        snap.put(
            &format!("{at}.medium"),
            match self.hello.medium {
                wire::Medium::Wired => "wired",
                wire::Medium::Wireless => "wireless",
            },
        );
        snap.put(&format!("{at}.sent"), self.sent);
        match self.said {
            Link::Down => snap.put(&format!("{at}.state"), "down"),
            Link::Unreported => snap.put(&format!("{at}.state"), "unreported"),
            Link::UpWithoutSpeed => snap.put(&format!("{at}.state"), "up"),
            Link::Up { mbps, full_duplex } => {
                snap.put(&format!("{at}.state"), "up");
                snap.put(&format!("{at}.speed_mbps"), mbps);
                snap.put(&format!("{at}.duplex"), if full_duplex { "full" } else { "half" });
            }
        }
        for (counter, value) in counters.into_iter().flat_map(Counters::iter) {
            snap.put(&format!("{at}.{}", counter.name()), value);
        }
    }
}

/// Each open link's transmit room in one opportunity, a frame of it spent as
/// the node hands the link one.
struct Rooms(Vec<(IfIndex, usize)>);

impl Credit for Rooms {
    fn room(&self, iface: IfIndex) -> bool {
        self.0.iter().any(|&(at, room)| at == iface && room > 0)
    }
    fn spend(&mut self, iface: IfIndex) {
        if let Some((_, room)) = self.0.iter_mut().find(|(at, _)| *at == iface) {
            *room = room.saturating_sub(1);
        }
    }
}

/// Offer the node every open link's transmit room until it has no frame left
/// or no link room, and ring each driver for what was sent. Returns each link
/// whose driver wrote what no driver writes, which is to end.
pub fn transmit(sessions: &mut [&mut Session], node: &mut Node, now: Instant, draw: fn() -> u32) -> Vec<(IfIndex, Fault)> {
    let mut faults: Vec<(IfIndex, Fault)> = Vec::new();
    loop {
        let mut rooms = Rooms(Vec::new());
        for session in sessions.iter_mut() {
            if faults.iter().any(|(at, _)| *at == session.iface) {
                continue;
            }
            match session.room() {
                Ok(room) => rooms.0.push((session.iface, room)),
                Err(why) => faults.push((session.iface, why)),
            }
        }
        let offered: usize = rooms.0.iter().map(|(_, room)| room).sum();
        if offered == 0 {
            return faults;
        }
        let sent = node.transmit(
            now,
            &mut rooms,
            |iface, frame| {
                let Some(session) = sessions.iter_mut().find(|s| s.iface == iface) else { unreachable!("the node sent by a link netstack does not hold") };
                if let Err(why) = session.send(frame)
                    && !faults.iter().any(|(at, _)| *at == iface)
                {
                    faults.push((iface, why));
                }
            },
            draw,
        );
        for session in sessions.iter_mut() {
            session.publish();
        }
        if sent < offered {
            return faults;
        }
    }
}
