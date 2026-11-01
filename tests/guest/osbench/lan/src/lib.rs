//! A bench's link to a peer, timed from the bench.
//!
//! **The bench finds the peer and opens every connection**: it broadcasts
//! [`HELLO`] to [`PORT`] and takes the source of the first answer as its
//! peer, so neither side is configured with an address and the bench accepts
//! nothing. The answer says which of its links the peer answered over
//! ([`link`]). The peer only answers; every figure is the bench's clock.
//!
//! Each TCP connection to the peer's [`PORT`] opens with one [`Request`]:
//! echo fixed-size messages, sink everything to end-of-file and answer the
//! count and its [`Ramp`], or source for a span and close.

use std::io::{self, ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub mod link;

/// The peer's UDP port for discovery and its TCP port for requests.
pub const PORT: u16 = 47_321;

/// What a bench broadcasts, and what the peer's answer starts with: the rest
/// is its link's description, at most [`MAX_SAID`] bytes of printable ASCII.
pub const HELLO: &[u8] = b"osbench-lan/3 hello";
pub const ANSWER: &[u8] = b"osbench-lan/3 peer ";
pub const MAX_SAID: usize = 160;

/// How long either end waits on one read or write before it gives the
/// connection up.
pub const PATIENCE: Duration = Duration::from_secs(10);

/// The bytes a transfer moves per call.
pub const CHUNK: usize = 64 * 1024;

/// The longest echoed message and the longest source span a peer serves.
pub const MAX_ECHO: u32 = CHUNK as u32;
pub const MAX_SOURCE_MS: u32 = 30_000;

/// What one connection asks of the peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// Echo every `size`-byte message until end-of-file.
    Echo { size: u32 },
    /// Read to end-of-file, then answer the count and then the [`Ramp`], each
    /// as eight little-endian bytes.
    Sink,
    /// Send for `millis`, then close.
    Source { millis: u32 },
}

const MAGIC: [u8; 4] = *b"OSBL";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    Magic,
    Op(u8),
    TooLarge(u32),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Magic => write!(f, "no osbench-lan request"),
            Refused::Op(op) => write!(f, "operation {op} is none"),
            Refused::TooLarge(n) => write!(f, "argument {n} is past what a peer serves"),
        }
    }
}

impl Request {
    pub const BYTES: usize = 9;

    pub fn encode(self) -> [u8; Self::BYTES] {
        let (op, arg) = match self {
            Request::Echo { size } => (1, size),
            Request::Sink => (2, 0),
            Request::Source { millis } => (3, millis),
        };
        let mut out = [0; Self::BYTES];
        out[..4].copy_from_slice(&MAGIC);
        out[4] = op;
        out[5..].copy_from_slice(&arg.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8; Self::BYTES]) -> Result<Self, Refused> {
        if bytes[..4] != MAGIC {
            return Err(Refused::Magic);
        }
        let arg = u32::from_le_bytes(bytes[5..].try_into().expect("four bytes"));
        let bounded = |n: u32, max: u32| if n == 0 || n > max { Err(Refused::TooLarge(n)) } else { Ok(n) };
        match bytes[4] {
            1 => Ok(Request::Echo { size: bounded(arg, MAX_ECHO)? }),
            2 => Ok(Request::Sink),
            3 => Ok(Request::Source { millis: bounded(arg, MAX_SOURCE_MS)? }),
            op => Err(Refused::Op(op)),
        }
    }
}

/// A peer that answered: where from, and what it said of its link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub ip: IpAddr,
    pub link: String,
}

/// The description an answer carries, or `None` for a datagram that is no
/// answer.
pub fn answered(datagram: &[u8]) -> Option<&str> {
    let said = datagram.strip_prefix(ANSWER)?;
    let printable = said.iter().all(|b| b.is_ascii_graphic() || *b == b' ');
    (said.len() <= MAX_SAID && printable).then(|| std::str::from_utf8(said).expect("ASCII"))
}

/// The bench's end: broadcast [`HELLO`] to `to` once a second until an answer
/// comes or `patience` has passed. `Ok(None)` is no answer; an error is a
/// socket the system would not give. Sends refused while the link has no
/// address yet are tried again.
pub fn discover(to: SocketAddr, patience: Duration) -> io::Result<Option<Found>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(Duration::from_secs(1)))?;
    let start = Instant::now();
    let mut buf = [0u8; ANSWER.len() + MAX_SAID + 1];
    while start.elapsed() < patience {
        if socket.send_to(HELLO, to).is_err() {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        let asked = Instant::now();
        while asked.elapsed() < Duration::from_secs(1) {
            match socket.recv_from(&mut buf) {
                Ok((n, from)) => {
                    if let Some(said) = answered(&buf[..n]) {
                        return Ok(Some(Found { ip: from.ip(), link: said.to_string() }));
                    }
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => break,
                Err(e) => return Err(e),
            }
        }
    }
    Ok(None)
}

/// A transfer's bytes and the bench's nanoseconds for them, and for an
/// upload what the peer had received by each of [`RAMP_MARKS_MS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transfer {
    pub bytes: u64,
    pub ns: u64,
    pub ramp: Option<Ramp>,
}

/// The milliseconds after a sink's first byte at which it reads its count: how
/// fast the sender's slow start opened, read the same way for every sender.
pub const RAMP_MARKS_MS: [u64; 4] = [100, 300, 1000, 2000];

/// Bytes a sink had received by each of [`RAMP_MARKS_MS`] after its first byte.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ramp(pub [u64; RAMP_MARKS_MS.len()]);

impl Ramp {
    const BYTES: usize = RAMP_MARKS_MS.len() * 8;

    fn encode(self) -> [u8; Self::BYTES] {
        let mut out = [0; Self::BYTES];
        for (at, bytes) in out.as_chunks_mut::<8>().0.iter_mut().zip(self.0) {
            *at = bytes.to_le_bytes();
        }
        out
    }

    fn decode(bytes: &[u8; Self::BYTES]) -> Self {
        let mut ramp = Self::default();
        for (to, at) in ramp.0.iter_mut().zip(bytes.as_chunks::<8>().0) {
            *to = u64::from_le_bytes(*at);
        }
        ramp
    }
}

/// A sink's count as it reads: the count at each mark is what arrived before
/// the read that passed it, and a mark the transfer ended before is the whole.
struct RampCount {
    first: Option<Instant>,
    ramp: Ramp,
    marked: usize,
}

impl RampCount {
    fn new() -> Self {
        Self { first: None, ramp: Ramp::default(), marked: 0 }
    }

    fn read(&mut self, count: u64) {
        let first = *self.first.get_or_insert_with(Instant::now);
        let since = first.elapsed();
        while self.marked < RAMP_MARKS_MS.len() && since >= Duration::from_millis(RAMP_MARKS_MS[self.marked]) {
            self.ramp.0[self.marked] = count;
            self.marked += 1;
        }
    }

    fn end(mut self, count: u64) -> Ramp {
        for at in &mut self.ramp.0[self.marked..] {
            *at = count;
        }
        self.ramp
    }
}

fn ns_since(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX).max(1)
}

/// The peer, as the bench reaches it.
pub struct Peer(pub SocketAddr);

impl Peer {
    fn open(&self, request: Option<Request>) -> io::Result<TcpStream> {
        let stream = TcpStream::connect_timeout(&self.0, PATIENCE)?;
        stream.set_read_timeout(Some(PATIENCE))?;
        stream.set_write_timeout(Some(PATIENCE))?;
        if let Some(request) = request {
            (&stream).write_all(&request.encode())?;
        }
        Ok(stream)
    }

    /// The local address a connection to the peer leaves from: which link
    /// carries it.
    pub fn local(&self) -> io::Result<SocketAddr> {
        self.open(None)?.local_addr()
    }

    /// A connection opened and closed.
    pub fn connect(&self) -> io::Result<()> {
        TcpStream::connect_timeout(&self.0, PATIENCE).map(drop)
    }

    /// A connection echoing `size`-byte messages, Nagle off.
    pub fn echo(&self, size: u32) -> io::Result<Echo> {
        let stream = self.open(Some(Request::Echo { size }))?;
        stream.set_nodelay(true)?;
        Ok(Echo { stream, buf: vec![0x5a; size as usize] })
    }

    /// Bytes written for `span`, timed from the first write to the peer's
    /// count of them, which must be all of them. `look` sees the connection
    /// once the last write is in, before its end is sent.
    pub fn upload(&self, span: Duration, look: &mut dyn FnMut(&TcpStream)) -> io::Result<Transfer> {
        let mut stream = self.open(Some(Request::Sink))?;
        let chunk = vec![0xa5u8; CHUNK];
        let t = Instant::now();
        let mut sent = 0u64;
        while t.elapsed() < span {
            stream.write_all(&chunk)?;
            sent += CHUNK as u64;
        }
        look(&stream);
        stream.shutdown(Shutdown::Write)?;
        let mut count = [0u8; 8];
        stream.read_exact(&mut count)?;
        let ns = ns_since(t);
        let mut ramp = [0u8; Ramp::BYTES];
        stream.read_exact(&mut ramp)?;
        let counted = u64::from_le_bytes(count);
        if counted != sent {
            return Err(io::Error::other(format!("the peer counted {counted} of the {sent} bytes sent")));
        }
        Ok(Transfer { bytes: sent, ns, ramp: Some(Ramp::decode(&ramp)) })
    }

    /// What the peer sends for `span` after its first read, timed from that
    /// read to end-of-file.
    pub fn download(&self, span: Duration) -> io::Result<Transfer> {
        let millis = u32::try_from(span.as_millis()).unwrap_or(u32::MAX);
        let mut stream = self.open(Some(Request::Source { millis }))?;
        let mut buf = vec![0u8; CHUNK];
        if stream.read(&mut buf)? == 0 {
            return Err(io::Error::other("the peer sent nothing"));
        }
        let mut bytes = 0u64;
        let t = Instant::now();
        loop {
            match stream.read(&mut buf)? {
                0 => break,
                n => bytes += n as u64,
            }
        }
        Ok(Transfer { bytes, ns: ns_since(t), ramp: None })
    }
}

/// An echoing connection.
pub struct Echo {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl Echo {
    /// One message there and back.
    pub fn round_trip(&mut self) -> io::Result<()> {
        self.stream.write_all(&self.buf)?;
        self.stream.read_exact(&mut self.buf)
    }
}

/// The most connections a peer serves at once.
pub const MAX_SESSIONS: usize = 8;

/// The peer's end of discovery: answer every [`HELLO`] on `socket` with the
/// link it answers over, for good.
pub fn answer(socket: &UdpSocket) -> io::Result<()> {
    let mut buf = [0u8; 64];
    loop {
        let (n, from) = socket.recv_from(&mut buf)?;
        if &buf[..n] == HELLO {
            let said = link::toward(from.ip()).map_or_else(|why| format!("unread: {why}"), |(_, said)| said);
            let mut reply = ANSWER.to_vec();
            reply.extend(said.bytes().filter(|b| b.is_ascii_graphic() || *b == b' ').take(MAX_SAID));
            socket.send_to(&reply, from)?;
        }
    }
}

/// The peer's end of requests: each connection on `listener` served on a
/// thread of its own, at most [`MAX_SESSIONS`] at once, for good. `said` is
/// told what each one asked or why it was refused.
pub fn serve(listener: &TcpListener, said: impl Fn(&str) + Send + Sync + 'static) -> io::Result<()> {
    let open = Arc::new(AtomicUsize::new(0));
    let said = Arc::new(said);
    loop {
        let (stream, _) = listener.accept()?;
        if open.fetch_add(1, Ordering::AcqRel) >= MAX_SESSIONS {
            open.fetch_sub(1, Ordering::AcqRel);
            said("a connection past the most served at once was closed");
            continue;
        }
        let (open, said) = (open.clone(), said.clone());
        std::thread::spawn(move || {
            if let Err(why) = session(stream, &*said) {
                said(&format!("a session ended: {why}"));
            }
            open.fetch_sub(1, Ordering::AcqRel);
        });
    }
}

fn session(mut stream: TcpStream, said: &dyn Fn(&str)) -> io::Result<()> {
    stream.set_read_timeout(Some(PATIENCE))?;
    stream.set_write_timeout(Some(PATIENCE))?;
    let mut header = [0u8; Request::BYTES];
    match stream.read_exact(&mut header) {
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(()),
        other => other?,
    }
    let request = Request::decode(&header).map_err(|why| io::Error::other(why.to_string()))?;
    said(&format!("serving {request:?}"));
    match request {
        Request::Echo { size } => {
            stream.set_nodelay(true)?;
            let mut buf = vec![0u8; size as usize];
            loop {
                match stream.read_exact(&mut buf) {
                    Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(()),
                    other => other?,
                }
                stream.write_all(&buf)?;
            }
        }
        Request::Sink => {
            let mut buf = vec![0u8; CHUNK];
            let mut count = 0u64;
            let mut ramp = RampCount::new();
            loop {
                match stream.read(&mut buf)? {
                    0 => break,
                    n => {
                        ramp.read(count);
                        count += n as u64;
                    }
                }
            }
            stream.write_all(&count.to_le_bytes())?;
            stream.write_all(&ramp.end(count).encode())
        }
        Request::Source { millis } => {
            let chunk = vec![0x3cu8; CHUNK];
            let t = Instant::now();
            while t.elapsed() < Duration::from_millis(u64::from(millis)) {
                stream.write_all(&chunk)?;
            }
            stream.shutdown(Shutdown::Write)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_reads_back_as_written() {
        for request in [Request::Echo { size: 64 }, Request::Sink, Request::Source { millis: MAX_SOURCE_MS }] {
            assert_eq!(Request::decode(&request.encode()), Ok(request));
        }
    }

    #[test]
    fn a_request_a_peer_does_not_serve_is_refused() {
        let mut bytes = Request::Sink.encode();
        bytes[0] = b'X';
        assert_eq!(Request::decode(&bytes), Err(Refused::Magic));
        let mut bytes = Request::Sink.encode();
        bytes[4] = 9;
        assert_eq!(Request::decode(&bytes), Err(Refused::Op(9)));
        let mut bytes = Request::Echo { size: 1 }.encode();
        bytes[5..].copy_from_slice(&(MAX_ECHO + 1).to_le_bytes());
        assert_eq!(Request::decode(&bytes), Err(Refused::TooLarge(MAX_ECHO + 1)));
        bytes[5..].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(Request::decode(&bytes), Err(Refused::TooLarge(0)));
        let mut bytes = Request::Source { millis: 1 }.encode();
        bytes[5..].copy_from_slice(&(MAX_SOURCE_MS + 1).to_le_bytes());
        assert_eq!(Request::decode(&bytes), Err(Refused::TooLarge(MAX_SOURCE_MS + 1)));
    }

    /// A peer on the loopback, its discovery socket's address and its
    /// requests' address.
    fn peer() -> (SocketAddr, SocketAddr) {
        let udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let tcp = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addrs = (udp.local_addr().unwrap(), tcp.local_addr().unwrap());
        std::thread::spawn(move || answer(&udp));
        std::thread::spawn(move || serve(&tcp, |_| {}));
        addrs
    }

    #[test]
    fn a_bench_finds_its_peer_and_measures_each_request() {
        let (discovery, requests) = peer();
        let found = discover(discovery, Duration::from_secs(5)).unwrap().expect("the peer answers");
        assert_eq!(found.ip, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert!(!found.link.is_empty(), "the answer says something of the peer's link");

        let peer = Peer(requests);
        peer.connect().unwrap();
        let mut echo = peer.echo(64).unwrap();
        for _ in 0..10 {
            echo.round_trip().unwrap();
        }
        let mut looked = 0;
        let up = peer.upload(Duration::from_millis(50), &mut |_| looked += 1).unwrap();
        assert!(up.bytes >= CHUNK as u64, "{up:?}");
        assert_eq!(looked, 1, "the upload shows its connection once");
        let ramp = up.ramp.expect("an upload carries its ramp").0;
        assert!(ramp.windows(2).all(|pair| pair[0] <= pair[1]) && ramp[3] == up.bytes, "{ramp:?}");
        let down = peer.download(Duration::from_millis(50)).unwrap();
        assert!(down.bytes >= CHUNK as u64, "{down:?}");
    }

    #[test]
    fn an_answer_carries_the_peers_link_and_nothing_else_is_one() {
        assert_eq!(answered(b"osbench-lan/3 peer wired driver=e1000e speed_mbps=1000"), Some("wired driver=e1000e speed_mbps=1000"));
        assert_eq!(answered(b"osbench-lan/3 peer "), Some(""));
        assert_eq!(answered(b"osbench-lan/1 peer"), None);
        assert_eq!(answered(b"osbench-lan/3 hello"), None);
        assert_eq!(answered(b"osbench-lan/3 peer wired\n"), None);
        assert_eq!(answered("osbench-lan/3 peer ä".as_bytes()), None);
        let mut long = ANSWER.to_vec();
        long.extend([b'x'; MAX_SAID]);
        assert!(answered(&long).is_some());
        long.push(b'x');
        assert_eq!(answered(&long), None);
    }

    #[test]
    fn no_answer_is_none_once_patience_runs_out() {
        let silent = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let found = discover(silent.local_addr().unwrap(), Duration::from_millis(1500)).unwrap();
        assert_eq!(found, None);
    }

    #[test]
    fn a_peer_closes_a_connection_whose_request_it_does_not_serve() {
        let (_, requests) = peer();
        let mut stream = TcpStream::connect(requests).unwrap();
        stream.set_read_timeout(Some(PATIENCE)).unwrap();
        let mut bad = Request::Echo { size: 1 }.encode();
        bad[5..].copy_from_slice(&(MAX_ECHO + 1).to_le_bytes());
        stream.write_all(&bad).unwrap();
        stream.write_all(&[0u8; 64]).unwrap();
        let mut got = Vec::new();
        let read = stream.read_to_end(&mut got);
        assert!(got.is_empty() && (read.is_ok() || read.unwrap_err().kind() == ErrorKind::ConnectionReset));
    }
}
