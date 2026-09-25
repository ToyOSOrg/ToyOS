//! The host as a neighbour on the guest's own Ethernet segment. A frame the
//! host writes arrives at the guest's NIC as if off the cable, and every frame
//! the guest sends reaches the host as well as slirp: QEMU's
//! `filter-redirector` puts the host's frames onto `net0` toward the guest,
//! and `filter-mirror` copies the guest's onto a second socket. Both speak
//! QEMU's `net_fill_rstate` framing: a 32-bit big-endian length, then the
//! frame, with no virtio header.
//!
//! What slirp's forward cannot do and this can: a frame's source is whatever
//! the host wrote, so a datagram can come from an on-link neighbour, or carry
//! a source no wire should.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Instant;

use toyos_build::icmp::checksum;

/// The two sockets QEMU serves the segment on, which [`super::qemu::BootOptions`]
/// carries into the argv.
#[derive(Clone, Debug)]
pub struct Tap {
    into_guest: PathBuf,
    from_guest: PathBuf,
}

impl Tap {
    /// Two socket paths of this boot's own, in this thread's scratch directory.
    pub fn in_lane() -> Self {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = super::lane::dir();
        let tap = Self {
            into_guest: dir.join(format!("tap-in-{n}.sock")),
            from_guest: dir.join(format!("tap-out-{n}.sock")),
        };
        let _ = std::fs::remove_file(&tap.into_guest);
        let _ = std::fs::remove_file(&tap.from_guest);
        tap
    }

    /// QEMU's half: two listening sockets, one filter each, both on `net0`. A
    /// filter on a netdev sees on its `tx` queue what the netdev sends toward
    /// the guest, and on `rx` what the guest sends it.
    pub fn argv(&self) -> [String; 8] {
        [
            "-chardev".into(),
            format!("socket,id=tapin,path={},server=on,wait=off", self.into_guest.display()),
            "-object".into(),
            "filter-redirector,id=tapinf,netdev=net0,queue=tx,indev=tapin".into(),
            "-chardev".into(),
            format!("socket,id=tapout,path={},server=on,wait=off", self.from_guest.display()),
            "-object".into(),
            "filter-mirror,id=tapoutf,netdev=net0,queue=rx,outdev=tapout".into(),
        ]
    }

    /// Stand on the segment of a guest booted with this tap. QEMU made both
    /// sockets before the machine ran, so both connects answer at once.
    pub fn open(&self) -> Result<Segment, String> {
        let connect = |path: &PathBuf| {
            UnixStream::connect(path).map_err(|e| format!("connect to QEMU's {}: {e}", path.display()))
        };
        let into = connect(&self.into_guest)?;
        let mut from = connect(&self.from_guest)?;
        let (tx, frames) = mpsc::channel();
        std::thread::spawn(move || {
            let mut len = [0u8; 4];
            while from.read_exact(&mut len).is_ok() {
                let mut frame = vec![0u8; u32::from_be_bytes(len) as usize];
                if from.read_exact(&mut frame).is_err() || tx.send(frame).is_err() {
                    return;
                }
            }
        });
        Ok(Segment { into, frames })
    }
}

/// A connection onto the segment.
pub struct Segment {
    into: UnixStream,
    frames: Receiver<Vec<u8>>,
}

impl Segment {
    /// Put `frame` on the segment, toward the guest.
    pub fn send(&mut self, frame: &[u8]) -> Result<(), String> {
        let len = u32::try_from(frame.len()).expect("a frame is shorter than 4 GiB").to_be_bytes();
        self.into
            .write_all(&len)
            .and_then(|()| self.into.write_all(frame))
            .map_err(|e| format!("put a frame on the segment: {e}"))
    }

    /// The next frame the guest sends, before `deadline`.
    pub fn next(&self, deadline: Instant) -> Result<Vec<u8>, String> {
        let left = deadline.saturating_duration_since(Instant::now());
        self.frames.recv_timeout(left).map_err(|e| match e {
            RecvTimeoutError::Timeout => "the guest sent no frame in time".to_string(),
            RecvTimeoutError::Disconnected => "QEMU closed the segment".to_string(),
        })
    }
}

const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_ARP: u16 = 0x0806;
const BROADCAST: [u8; 6] = [0xff; 6];

/// RFC 826: who has `target`, asked by `mac` at `ip`, broadcast.
pub fn arp_request(mac: [u8; 6], ip: [u8; 4], target: [u8; 4]) -> Vec<u8> {
    let mut frame = ethernet(BROADCAST, mac, ETHERTYPE_ARP);
    // Ethernet, IPv4, 6- and 4-byte addresses, a request.
    frame.extend_from_slice(&[0, 1, 0x08, 0x00, 6, 4, 0, 1]);
    frame.extend_from_slice(&mac);
    frame.extend_from_slice(&ip);
    frame.extend_from_slice(&[0; 6]);
    frame.extend_from_slice(&target);
    // The shortest Ethernet frame, less its FCS.
    frame.resize(60, 0);
    frame
}

/// The hardware address of the ARP reply in `frame` saying where `ip` is, if
/// that is what `frame` is.
pub fn arp_reply_for(frame: &[u8], ip: [u8; 4]) -> Option<[u8; 6]> {
    let arp = frame.get(14..42)?;
    let is_reply = u16_at(frame, 12) == Some(ETHERTYPE_ARP) && arp[6..8] == [0, 2];
    (is_reply && arp[14..18] == ip).then(|| arp[8..14].try_into().expect("six bytes"))
}

/// One UDP datagram in one IPv4 packet (RFC 791, RFC 768), with both checksums.
pub struct Udp<'a> {
    pub dst_mac: [u8; 6],
    pub src_mac: [u8; 6],
    pub src: ([u8; 4], u16),
    pub dst: ([u8; 4], u16),
    pub payload: &'a [u8],
}

impl Udp<'_> {
    pub fn frame(&self) -> Vec<u8> {
        let udp_len = 8 + self.payload.len();
        let mut ip = vec![0x45, 0];
        ip.extend_from_slice(&(20 + udp_len as u16).to_be_bytes());
        // ID, no fragment, TTL 64 (§3.2 ignores it on one link), UDP.
        ip.extend_from_slice(&[0, 0, 0x40, 0, 64, 17, 0, 0]);
        ip.extend_from_slice(&self.src.0);
        ip.extend_from_slice(&self.dst.0);
        let sum = checksum(&ip).to_be_bytes();
        ip[10..12].copy_from_slice(&sum);

        let mut udp = Vec::with_capacity(udp_len);
        udp.extend_from_slice(&self.src.1.to_be_bytes());
        udp.extend_from_slice(&self.dst.1.to_be_bytes());
        udp.extend_from_slice(&(udp_len as u16).to_be_bytes());
        udp.extend_from_slice(&[0, 0]);
        udp.extend_from_slice(self.payload);
        let mut pseudo = Vec::with_capacity(12 + udp_len);
        pseudo.extend_from_slice(&self.src.0);
        pseudo.extend_from_slice(&self.dst.0);
        pseudo.extend_from_slice(&[0, 17]);
        pseudo.extend_from_slice(&(udp_len as u16).to_be_bytes());
        pseudo.extend_from_slice(&udp);
        // RFC 768: a computed zero is sent as all ones.
        let sum = match checksum(&pseudo) {
            0 => 0xffff,
            sum => sum,
        };
        udp[6..8].copy_from_slice(&sum.to_be_bytes());

        let mut frame = ethernet(self.dst_mac, self.src_mac, ETHERTYPE_IPV4);
        frame.extend_from_slice(&ip);
        frame.extend_from_slice(&udp);
        frame.resize(frame.len().max(60), 0);
        frame
    }
}

/// The UDP datagram `frame` carries, if it carries one in an IPv4 packet with
/// no options: its addresses, and its payload.
pub fn udp_in(frame: &[u8]) -> Option<Udp<'_>> {
    if u16_at(frame, 12)? != ETHERTYPE_IPV4 || *frame.get(14)? != 0x45 || *frame.get(23)? != 17 {
        return None;
    }
    let ip_len = u16_at(frame, 16)? as usize;
    let udp_len = u16_at(frame, 38)? as usize;
    if udp_len < 8 || 20 + udp_len > ip_len {
        return None;
    }
    Some(Udp {
        dst_mac: frame[0..6].try_into().ok()?,
        src_mac: frame[6..12].try_into().ok()?,
        src: (frame[26..30].try_into().ok()?, u16_at(frame, 34)?),
        dst: (frame[30..34].try_into().ok()?, u16_at(frame, 36)?),
        payload: frame.get(42..34 + udp_len)?,
    })
}

fn ethernet(dst: [u8; 6], src: [u8; 6], ethertype: u16) -> Vec<u8> {
    let mut frame = Vec::with_capacity(64);
    frame.extend_from_slice(&dst);
    frame.extend_from_slice(&src);
    frame.extend_from_slice(&ethertype.to_be_bytes());
    frame
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}
