//! A wire between the guest and slirp that this host impairs: every frame
//! either way passes through here, and a [`Plan`] says which do not.
//!
//! QEMU's `filter-redirector` takes each frame off `net0` in one direction,
//! writes it to a socket this module reads, and puts each frame this module
//! writes back onto `net0` in the same direction, past the filter. Both
//! sockets speak `super::segment`'s framing. A filter sees on its `tx` queue
//! what the netdev sends toward the guest, and on `rx` what the guest sends
//! it.
//!
//! **Every impairment is a count or a switch, never a chance**, so no run can
//! go without it.

use std::collections::HashSet;
use std::io::{ErrorKind, Read};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use toyos_build::icmp::checksum;

use super::segment::{dns_answer, read_frame, udp_in, write_frame, Udp, RESOLVER};

/// Every this-many data segments one way, [`Plan::Impair`] drops one.
pub const DROP_EVERY: u64 = 53;
/// Every this-many, [`Plan::Impair`] holds one back behind the next.
pub const SWAP_EVERY: u64 = 31;

/// How long a held segment waits for one to follow it before it is sent
/// anyway: the last segment of a burst has none behind it.
const HOLD_AT_MOST: Duration = Duration::from_millis(20);

/// [`Plan::Answer`]'s flood: one echo request toward the guest this often,
/// with this identifier.
const FLOOD_EVERY: Duration = Duration::from_micros(500);
const FLOOD_ID: u16 = 0x7f1d;

/// What the wire does to the frames through it.
#[derive(Clone, Debug)]
pub enum Plan {
    /// Every [`DROP_EVERY`]th data segment each way is dropped and every
    /// [`SWAP_EVERY`]th held back behind the next, retransmissions counted
    /// with the rest. Every other frame passes untouched and in order.
    Impair,
    /// Everything passes but a query to slirp's resolver, which is answered
    /// here (`segment::dns_answer`) and goes no further; where
    /// `drop_guest_fins`, the first transmission of every FIN the guest sends,
    /// so each connection it closes finishes only on its own retransmission
    /// timer; and where `flood`, from the guest's first addressed frame until
    /// the wire closes, an echo request from slirp's gateway toward it every
    /// [`FLOOD_EVERY`], whose replies are counted here and go no further.
    Answer { drop_guest_fins: bool, flood: bool },
    /// Everything passes until the switch is thrown, and nothing after it,
    /// either way, ARP included: every peer of the guest's vanishes at once.
    Dark(Arc<AtomicBool>),
}

/// The four socket paths QEMU connects the two directions to.
#[derive(Clone, Debug)]
pub struct Wire {
    dir: PathBuf,
    n: u32,
}

/// One direction's filter: `queue`, and its two sockets' names.
const DIRECTIONS: [(&str, &str); 2] = [("tx", "toguest"), ("rx", "fromguest")];

impl Wire {
    /// Listen on four socket paths of this boot's own, in this thread's
    /// scratch directory, and stand between the guest and slirp from the
    /// moment QEMU connects to them.
    ///
    /// **This side listens and QEMU connects**, at its own startup and before
    /// the machine runs, so no frame reaches a filter before this module is
    /// there to take it.
    pub fn listen(plan: Plan) -> Result<(Self, Impaired), String> {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let wire = Self { dir: super::lane::dir(), n: SEQ.fetch_add(1, Ordering::Relaxed) };
        let (to_guest, toward_guest) = mpsc::channel();
        let (from_guest, away_from_guest) = mpsc::channel();
        // Toward the guest, and from it with a way to answer it.
        let lanes = [(toward_guest, to_guest.clone(), None), (away_from_guest, from_guest, Some(to_guest))];
        let mut directions = Vec::new();
        for ((way, (_, name)), (queue, pass, answer)) in DIRECTIONS.into_iter().enumerate().zip(lanes) {
            let bind = |end: &str| {
                let path = wire.path(name, end);
                let _ = std::fs::remove_file(&path);
                UnixListener::bind(&path).map_err(|e| format!("listen on {}: {e}", path.display()))
            };
            let (out, back) = (bind("out")?, bind("in")?);
            let plan = plan.clone();
            directions.push(thread::spawn(move || {
                let from = out.accept().map_err(|e| format!("QEMU's frames: {e}"))?.0;
                let to = back.accept().map_err(|e| format!("QEMU's way back: {e}"))?.0;
                impair(from, to, Lane { queue, pass, answer }, &plan, way == 1)
            }));
        }
        let paths = DIRECTIONS
            .iter()
            .flat_map(|(_, name)| [wire.path(name, "out"), wire.path(name, "in")])
            .collect();
        Ok((wire, Impaired { directions, paths }))
    }

    fn path(&self, name: &str, end: &str) -> PathBuf {
        self.dir.join(format!("mb-{name}-{end}-{}.sock", self.n))
    }

    /// QEMU's half: per direction, one filter whose `outdev` hands this module
    /// every frame and whose `indev` takes back the ones it passes on.
    pub fn argv(&self) -> Vec<String> {
        let mut argv = Vec::new();
        for (queue, name) in DIRECTIONS {
            for end in ["out", "in"] {
                argv.push("-chardev".into());
                argv.push(format!("socket,id=mb{name}{end},path={},server=off", self.path(name, end).display()));
            }
            argv.push("-object".into());
            argv.push(format!(
                "filter-redirector,id=mb{name},netdev=net0,queue={queue},outdev=mb{name}out,indev=mb{name}in"
            ));
        }
        argv
    }
}

/// What one direction did to the frames through it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Counts {
    pub frames: u64,
    /// TCP segments carrying data.
    pub data: u64,
    pub dropped: u64,
    pub swapped: u64,
    /// Data segments whose sequence number an earlier one in this direction
    /// already carried: the sender's retransmissions.
    pub resent: u64,
    /// FINs dropped by [`Plan::Answer`].
    pub fins_dropped: u64,
    /// Queries to slirp's resolver [`Plan::Answer`] answered.
    pub resolved: u64,
    /// Frames the dark swallowed.
    pub darkened: u64,
    /// Echo requests the flood sent, replies to them, and replies that came
    /// after a later request's.
    pub flooded: u64,
    pub echoed: u64,
    pub echoes_reordered: u64,
}

pub struct Impaired {
    directions: Vec<JoinHandle<Result<Counts, String>>>,
    paths: Vec<PathBuf>,
}

impl Impaired {
    /// The counts toward the guest and from it, once QEMU has closed both
    /// directions — which it does when it exits, so call this after.
    ///
    /// A connection of this function's own to every socket first: an accept
    /// QEMU never answered takes it and ends its direction with nothing
    /// counted, rather than waiting for ever.
    pub fn finish(self) -> Result<[Counts; 2], String> {
        for path in &self.paths {
            let _ = UnixStream::connect(path);
        }
        let mut counts = [Counts::default(); 2];
        for (i, direction) in self.directions.into_iter().enumerate() {
            counts[i] = direction.join().map_err(|_| "a middlebox direction panicked".to_string())??;
        }
        Ok(counts)
    }
}

/// The TCP segment a frame carries in an IPv4 packet: its ports and sequence
/// number, whether it carries data and whether it carries a FIN.
struct Segment {
    key: (u16, u16, u32),
    data: bool,
    fin: bool,
}

fn tcp_segment(frame: &[u8]) -> Option<Segment> {
    let be16 = |at: usize| frame.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    if be16(12)? != 0x0800 || *frame.get(23)? != 6 {
        return None;
    }
    let ihl = (*frame.get(14)? & 0x0f) as usize * 4;
    let total = be16(16)? as usize;
    let tcp = 14 + ihl;
    let doff = (*frame.get(tcp + 12)? >> 4) as usize * 4;
    let seq = u32::from_be_bytes(frame.get(tcp + 4..tcp + 8)?.try_into().ok()?);
    let fin = *frame.get(tcp + 13)? & 1 != 0;
    Some(Segment { key: (be16(tcp)?, be16(tcp + 2)?, seq), data: total > ihl + doff, fin })
}

/// What one read off QEMU's socket found.
enum Next {
    Frame(Vec<u8>),
    /// Nothing within the read timeout.
    Idle,
    /// QEMU closed the socket, which it does when it exits.
    Closed,
}

/// The next frame, or [`Next::Idle`] if none starts within `wait`. Only the
/// first byte is waited for with a timeout: the frame is already on its way
/// behind it, and waits as long as it takes.
fn next(from: &mut UnixStream, wait: Option<Duration>) -> Result<Next, String> {
    from.set_read_timeout(wait).map_err(|e| format!("{e}"))?;
    let mut first = [0u8; 1];
    match from.read(&mut first) {
        Ok(0) => return Ok(Next::Closed),
        Ok(_) => {}
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Ok(Next::Idle),
        Err(e) if e.kind() == ErrorKind::ConnectionReset => return Ok(Next::Closed),
        Err(e) => return Err(format!("wait for a frame: {e}")),
    }
    from.set_read_timeout(None).map_err(|e| format!("{e}"))?;
    match read_frame(&mut (&first[..]).chain(&mut *from)) {
        Ok(frame) => Ok(Next::Frame(frame)),
        Err(e) if matches!(e.kind(), ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset) => Ok(Next::Closed),
        Err(e) => Err(format!("read a frame: {e}")),
    }
}

/// One direction's queues: what its writer hands QEMU, the way into that
/// queue, and — from the guest — the way into the queue toward it.
struct Lane {
    queue: Receiver<Vec<u8>>,
    pass: Sender<Vec<u8>>,
    answer: Option<Sender<Vec<u8>>>,
}

/// One direction, until QEMU closes it. `from_guest` says which.
///
/// **Reading from QEMU never waits on writing to it.** QEMU writes a frame to
/// a filter's socket with a blocking write from its main loop, which is also
/// what reads the frames coming back: a direction that stopped reading while
/// its own write back was blocked would stop QEMU, and QEMU the other
/// direction's writer, for ever. So what passes goes through a queue to a
/// writer of its own.
fn impair(mut from: UnixStream, mut to: UnixStream, lane: Lane, plan: &Plan, from_guest: bool) -> Result<Counts, String> {
    let Lane { queue, pass, answer } = lane;
    let writer = thread::spawn(move || -> Result<(), String> {
        for frame in queue {
            write_frame(&mut to, &frame).map_err(|e| format!("pass a frame on: {e}"))?;
        }
        Ok(())
    });
    let counts = sort(&mut from, &pass, answer.as_ref(), plan, from_guest);
    drop((pass, answer));
    // A writer refused by a QEMU that has exited is that exit, which the
    // reader has already seen as its socket closing.
    let _ = writer.join().map_err(|_| "a middlebox writer panicked".to_string())?;
    counts
}

/// Decide each frame's fate, and pass on the ones that go.
fn sort(
    from: &mut UnixStream,
    pass: &Sender<Vec<u8>>,
    answer: Option<&Sender<Vec<u8>>>,
    plan: &Plan,
    from_guest: bool,
) -> Result<Counts, String> {
    let gone = |_| "the middlebox writer has gone".to_string();
    let send = |frame: Vec<u8>| pass.send(frame).map_err(gone);
    let mut counts = Counts::default();
    let mut seen = HashSet::new();
    let mut held: Option<Vec<u8>> = None;
    let (mut flood, mut last_echo) = (None, 0);
    loop {
        let frame = match next(from, held.as_ref().map(|_| HOLD_AT_MOST))? {
            Next::Frame(frame) => frame,
            Next::Closed => {
                counts.flooded = flood.map_or(Ok(0), JoinHandle::join).map_err(|_| "the flood panicked".to_string())?;
                return Ok(counts);
            }
            Next::Idle => {
                send(held.take().expect("a timeout is set only while a segment is held"))?;
                continue;
            }
        };
        counts.frames += 1;
        let segment = tcp_segment(&frame);
        match plan {
            Plan::Dark(switch) => {
                if switch.load(Ordering::Acquire) {
                    counts.darkened += 1;
                } else {
                    send(frame)?;
                }
            }
            Plan::Answer { drop_guest_fins, flood: flooding } if from_guest => {
                if let (true, None, Some(answer), Some(ip)) = (*flooding, &flood, answer, guest_ip(&frame)) {
                    let (answer, macs) = (answer.clone(), [&frame[6..12], &frame[0..6]].concat());
                    flood = Some(thread::spawn(move || flood_toward(&macs, ip, &answer)));
                }
                if let Some(seq) = echo_reply(&frame) {
                    counts.echoed += 1;
                    // RFC 1982's order, which a sequence number that wrapped keeps.
                    let behind = (seq.wrapping_sub(last_echo) as i16) < 0;
                    counts.echoes_reordered += behind as u64;
                    last_echo = if behind { last_echo } else { seq };
                    continue;
                }
                if let Some(query) = udp_in(&frame).filter(|u| u.dst == (RESOLVER, 53)) {
                    if let (Some(payload), Some(answer)) = (dns_answer(query.payload), answer) {
                        counts.resolved += 1;
                        let reply =
                            Udp { dst_mac: query.src_mac, src_mac: query.dst_mac, src: query.dst, dst: query.src, payload: &payload };
                        answer.send(reply.frame()).map_err(gone)?;
                    }
                    continue;
                }
                if *drop_guest_fins && segment.is_some_and(|s| s.fin && seen.insert(s.key)) {
                    counts.fins_dropped += 1;
                    continue;
                }
                send(frame)?;
            }
            Plan::Answer { .. } => send(frame)?,
            Plan::Impair => {
                let Some(segment) = segment.filter(|s| s.data) else {
                    send(frame)?;
                    continue;
                };
                counts.data += 1;
                if !seen.insert(segment.key) {
                    counts.resent += 1;
                }
                if counts.data % DROP_EVERY == 0 {
                    counts.dropped += 1;
                    continue;
                }
                if counts.data % SWAP_EVERY == 0 && held.is_none() {
                    counts.swapped += 1;
                    held = Some(frame);
                    continue;
                }
                send(frame)?;
                if let Some(late) = held.take() {
                    send(late)?;
                }
            }
        }
    }
}

/// The guest's address, if `frame` is a unicast IPv4 frame it sent from one:
/// its source and destination hardware addresses are then the guest's and
/// its gateway's.
fn guest_ip(frame: &[u8]) -> Option<[u8; 4]> {
    let ip: [u8; 4] = frame.get(26..30)?.try_into().ok()?;
    (frame.get(12..14)? == [0x08, 0x00] && frame[0] & 1 == 0 && ip != [0; 4]).then_some(ip)
}

/// [`Plan::Answer`]'s flood toward the guest at `ip_of_guest`, framed with
/// `macs` (the guest's, then the gateway's), until the lane toward it closes;
/// answers how many requests went. **The pace is the stimulus**, so each
/// request is slept to at its own moment from the first.
fn flood_toward(macs: &[u8], ip_of_guest: [u8; 4], to_guest: &Sender<Vec<u8>>) -> u64 {
    let started = Instant::now();
    for seq in 0u32.. {
        thread::sleep((started + FLOOD_EVERY * seq).saturating_duration_since(Instant::now()));
        // RFC 792's echo request, from slirp's gateway `10.0.2.2`.
        let mut icmp = [8, 0, 0, 0, 0, 0, 0, 0];
        icmp[4..6].copy_from_slice(&FLOOD_ID.to_be_bytes());
        icmp[6..8].copy_from_slice(&(seq as u16).to_be_bytes());
        let sum = checksum(&icmp).to_be_bytes();
        icmp[2..4].copy_from_slice(&sum);
        let mut ip = vec![0x45, 0, 0, 28, 0, 0, 0x40, 0, 64, 1, 0, 0, 10, 0, 2, 2];
        ip.extend_from_slice(&ip_of_guest);
        let sum = checksum(&ip).to_be_bytes();
        ip[10..12].copy_from_slice(&sum);
        let mut frame = [macs, &[0x08, 0x00], &ip, &icmp].concat();
        // The shortest Ethernet frame, less its FCS.
        frame.resize(60, 0);
        if to_guest.send(frame).is_err() {
            return seq.into();
        }
    }
    unreachable!("a flood outlived four billion requests")
}

/// The sequence number of the reply to one of the flood's echo requests
/// `frame` carries, if it carries one.
fn echo_reply(frame: &[u8]) -> Option<u16> {
    let be16 = |at: usize| frame.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let icmp = 14 + (*frame.get(14)? & 0x0f) as usize * 4;
    let reply = be16(12)? == 0x0800 && *frame.get(23)? == 1 && *frame.get(icmp)? == 0 && be16(icmp + 4)? == FLOOD_ID;
    reply.then_some(be16(icmp + 6)?)
}
