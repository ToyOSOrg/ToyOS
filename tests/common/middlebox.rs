//! An impaired wire between the guest and slirp: every frame either way
//! passes through this host, which drops some of the TCP segments carrying
//! data and swaps others with the one after, so a stream is exact only if
//! both TCP stacks retransmit and reassemble.
//!
//! QEMU's `filter-redirector` takes each frame off `net0` in one direction,
//! writes it to a socket this module reads, and puts each frame this module
//! writes back onto `net0` in the same direction, past the filter. Both
//! sockets speak QEMU's `net_fill_rstate` framing: a 32-bit big-endian length,
//! then the frame, with no virtio header. A filter sees on its `tx` queue what
//! the netdev sends toward the guest, and on `rx` what the guest sends it.
//!
//! **The impairment is a count, not a chance**: every [`DROP_EVERY`]th data
//! segment each way is dropped and every [`SWAP_EVERY`]th held back behind the
//! next one, retransmissions counted with the rest, so no run can go without
//! losses. Every other frame — ARP, DHCP, a bare ACK — passes untouched and in
//! order.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Every this-many data segments one way, one is dropped.
pub const DROP_EVERY: u64 = 53;
/// Every this-many, one is held back behind the next.
pub const SWAP_EVERY: u64 = 31;

/// How long a held segment waits for one to follow it before it is sent
/// anyway: the last segment of a burst has none behind it.
const HOLD_AT_MOST: Duration = Duration::from_millis(20);

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
    pub fn listen() -> Result<(Self, Impaired), String> {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let wire = Self { dir: super::lane::dir(), n: SEQ.fetch_add(1, Ordering::Relaxed) };
        let mut directions = Vec::new();
        for (_, name) in DIRECTIONS {
            let bind = |end: &str| {
                let path = wire.path(name, end);
                let _ = std::fs::remove_file(&path);
                UnixListener::bind(&path).map_err(|e| format!("listen on {}: {e}", path.display()))
            };
            let (out, back) = (bind("out")?, bind("in")?);
            directions.push(thread::spawn(move || {
                let from = out.accept().map_err(|e| format!("QEMU's frames: {e}"))?.0;
                let to = back.accept().map_err(|e| format!("QEMU's way back: {e}"))?.0;
                impair(from, to)
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

/// The TCP segment a frame carries, if it carries data in an IPv4 packet:
/// its ports and its sequence number.
fn data_segment(frame: &[u8]) -> Option<(u16, u16, u32)> {
    let be16 = |at: usize| frame.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    if be16(12)? != 0x0800 || *frame.get(23)? != 6 {
        return None;
    }
    let ihl = (*frame.get(14)? & 0x0f) as usize * 4;
    let total = be16(16)? as usize;
    let tcp = 14 + ihl;
    let doff = (*frame.get(tcp + 12)? >> 4) as usize * 4;
    if total <= ihl + doff {
        return None;
    }
    let seq = u32::from_be_bytes(frame.get(tcp + 4..tcp + 8)?.try_into().ok()?);
    Some((be16(tcp)?, be16(tcp + 2)?, seq))
}

/// What one read off QEMU's socket found.
enum Next {
    Frame(Vec<u8>),
    /// Nothing within the read timeout.
    Idle,
    /// QEMU closed the socket, which it does when it exits.
    Closed,
}

fn read_frame(from: &mut UnixStream) -> Result<Next, String> {
    let mut len = [0u8; 4];
    match from.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if matches!(e.kind(), ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset) => return Ok(Next::Closed),
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Ok(Next::Idle),
        Err(e) => return Err(format!("read a frame's length: {e}")),
    }
    let mut frame = vec![0u8; u32::from_be_bytes(len) as usize];
    // The frame is already on its way behind its length, and waits as long as
    // it takes.
    from.set_read_timeout(None).map_err(|e| format!("{e}"))?;
    match from.read_exact(&mut frame) {
        Ok(()) => Ok(Next::Frame(frame)),
        Err(e) if matches!(e.kind(), ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset) => Ok(Next::Closed),
        Err(e) => Err(format!("read a frame: {e}")),
    }
}

fn write_frame(to: &mut UnixStream, frame: &[u8]) -> Result<(), String> {
    let len = u32::try_from(frame.len()).expect("a frame is shorter than 4 GiB").to_be_bytes();
    to.write_all(&len).and_then(|()| to.write_all(frame)).map_err(|e| format!("pass a frame on: {e}"))
}

/// One direction, until QEMU closes it.
///
/// **Reading from QEMU never waits on writing to it.** QEMU writes a frame to
/// a filter's socket with a blocking write from its main loop, which is also
/// what reads the frames coming back: a direction that stopped reading while
/// its own write back was blocked would stop QEMU, and QEMU the other
/// direction's writer, for ever. So what passes goes through a queue to a
/// writer of its own.
fn impair(mut from: UnixStream, mut to: UnixStream) -> Result<Counts, String> {
    let (pass, passed) = std::sync::mpsc::channel::<Vec<u8>>();
    let writer = thread::spawn(move || -> Result<(), String> {
        for frame in passed {
            write_frame(&mut to, &frame)?;
        }
        Ok(())
    });
    let counts = sort(&mut from, &pass);
    drop(pass);
    // A writer refused by a QEMU that has exited is that exit, which the
    // reader has already seen as its socket closing.
    let _ = writer.join().map_err(|_| "a middlebox writer panicked".to_string())?;
    counts
}

/// Decide each frame's fate, and pass on the ones that go.
fn sort(from: &mut UnixStream, pass: &std::sync::mpsc::Sender<Vec<u8>>) -> Result<Counts, String> {
    let send = |frame: Vec<u8>| pass.send(frame).map_err(|_| "the middlebox writer has gone".to_string());
    let mut counts = Counts::default();
    let mut seen = std::collections::HashSet::new();
    let mut held: Option<Vec<u8>> = None;
    loop {
        from.set_read_timeout(held.as_ref().map(|_| HOLD_AT_MOST)).map_err(|e| format!("{e}"))?;
        let frame = match read_frame(from)? {
            Next::Frame(frame) => frame,
            Next::Closed => return Ok(counts),
            Next::Idle => {
                send(held.take().expect("a timeout is set only while a segment is held"))?;
                continue;
            }
        };
        counts.frames += 1;
        let Some(segment) = data_segment(&frame) else {
            send(frame)?;
            continue;
        };
        counts.data += 1;
        if !seen.insert(segment) {
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
