//! A stream's life on a wire: netd's connection on Netstack3's core, its peer a
//! second core on the far end of a link these tests can cut, both on one clock
//! the tests move, and the client's two pipes played here, so what is judged is
//! what the client and the peer each see.

use super::*;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::num::{NonZeroU16, NonZeroUsize};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use net_types::ip::Ipv4Addr;
use net_types::{SpecifiedAddr, ZonedAddr};

use crate::net::Address;
use crate::stack::{core_limits, Clock, SocketExtra};

const NETD: ([u8; 4], [u8; 6]) = ([10, 0, 0, 2], [2, 0, 0, 0, 0, 2]);
const PEER: ([u8; 4], [u8; 6]) = ([10, 0, 0, 3], [2, 0, 0, 0, 0, 3]);
const PORT: u16 = 80;
/// How far one pass moves the clock.
const TICK: Duration = Duration::from_millis(100);

/// The host's randomness, which the kernel's stands for here: a new key per
/// draw from std's own seeded hasher.
fn host_random(dest: &mut [u8]) {
    use std::hash::{BuildHasher, Hasher};
    for chunk in dest.chunks_mut(8) {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(0);
        chunk.copy_from_slice(&hasher.finish().to_le_bytes()[..chunk.len()]);
    }
}

/// What the link does with a frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Link {
    Up,
    /// Every frame either way is lost: a peer that has gone.
    Lost,
    /// netd's next frame that carries TCP payload is lost, and every other
    /// frame passes.
    LoseNextData,
}

/// One direction of a client's pipe pair, as the kernel's ring behaves.
struct Ring {
    bytes: VecDeque<u8>,
    cap: usize,
    writer: bool,
    reader: bool,
}

type Pipe = Rc<RefCell<Ring>>;

fn ring(cap: usize) -> Pipe {
    Rc::new(RefCell::new(Ring { bytes: VecDeque::new(), cap, writer: true, reader: true }))
}

/// netd's end of one of the pair, which writes (the receive pipe) or reads
/// (the send pipe), and says in `closed` when it is dropped.
struct NetdEnd {
    ring: Pipe,
    writes: bool,
    closed: Rc<RefCell<Vec<&'static str>>>,
}

impl ToClient for NetdEnd {
    fn write(&self, bytes: &[u8]) -> Result<usize, SyscallError> {
        assert!(self.writes, "netd wrote into its send pipe");
        let mut r = self.ring.borrow_mut();
        if !r.reader {
            return Err(SyscallError::Gone);
        }
        let n = bytes.len().min(r.cap - r.bytes.len());
        if n == 0 && !bytes.is_empty() {
            return Err(SyscallError::WouldBlock);
        }
        r.bytes.extend(&bytes[..n]);
        Ok(n)
    }
}

impl FromClient for NetdEnd {
    fn writer_gone(&self) -> bool {
        !self.ring.borrow().writer
    }

    fn read(&self, buf: &mut [u8]) -> Result<usize, SyscallError> {
        assert!(!self.writes, "netd read its receive pipe");
        let mut r = self.ring.borrow_mut();
        if r.bytes.is_empty() {
            return if r.writer { Err(SyscallError::WouldBlock) } else { Ok(0) };
        }
        let n = buf.len().min(r.bytes.len());
        for (b, byte) in buf.iter_mut().zip(r.bytes.drain(..n)) {
            *b = byte;
        }
        Ok(n)
    }
}

impl Drop for NetdEnd {
    fn drop(&mut self) {
        let mut r = self.ring.borrow_mut();
        if self.writes {
            r.writer = false;
            self.closed.borrow_mut().push("receive");
        } else {
            r.reader = false;
            self.closed.borrow_mut().push("send");
        }
    }
}

/// A stack on the shared clock, holding `address`.
fn stack(clock: &Arc<AtomicU64>, (addr, mac): ([u8; 4], [u8; 6])) -> Net {
    let mut net = Net::new(mac, Clock::Moved(Arc::clone(clock)), host_random);
    net.apply(Some(Address { addr, prefix: 24, router: None })).expect("a /24 address");
    net
}

fn to(addr: [u8; 4]) -> Option<ZonedAddr<SpecifiedAddr<Ipv4Addr>, netstack3_core::device::DeviceId<Bindings>>> {
    SpecifiedAddr::new(Ipv4Addr::new(addr)).map(ZonedAddr::Unzoned)
}

/// Whether `frame` is IPv4 TCP carrying payload or a FIN, and its sequence
/// number.
fn tcp_segment(frame: &[u8]) -> Option<(u32, bool)> {
    if frame.len() < 14 + 20 + 20 || frame[12..14] != [0x08, 0x00] || frame[14 + 9] != 6 {
        return None;
    }
    let ihl = usize::from(frame[14] & 0x0f) * 4;
    let total = usize::from(u16::from_be_bytes([frame[16], frame[17]]));
    let tcp = &frame[14 + ihl..14 + total];
    let data_offset = usize::from(tcp[12] >> 4) * 4;
    let seq = u32::from_be_bytes([tcp[4], tcp[5], tcp[6], tcp[7]]);
    let carries = tcp.len() > data_offset || tcp[13] & 0x01 != 0;
    Some((seq, carries))
}

/// netd's connection, its peer's, the link between them, and the client.
///
/// **The two stacks are its last fields**, so every socket id drops before
/// the stack that holds it: the core refuses to drop a socket a strong id
/// still names.
struct World {
    clock: Arc<AtomicU64>,
    born: Instant,
    link: Link,
    peer_id: TcpId,
    peer_shared: Arc<Shared>,
    /// Whether the peer reads what arrives, which is what opens its window.
    peer_reads: bool,
    peer_got: Vec<u8>,
    /// Frames the link has dropped on the way to the peer.
    lost: usize,
    conn: Option<PipedConnection<NetdEnd, NetdEnd>>,
    closing: Option<Closing>,
    /// Whether netd's closing table has room for this stream once its client
    /// is done with it.
    room: bool,
    to_client: Pipe,
    from_client: Pipe,
    /// netd's ends, in the order netd closed them.
    closed: Rc<RefCell<Vec<&'static str>>>,
    netd: Net,
    peer: Net,
}

impl World {
    /// A stream established to a listening peer, its client holding both
    /// ends of a pair of pipes `pipe` bytes deep.
    fn new(pipe: usize) -> Self {
        let clock = Arc::new(AtomicU64::new(0));
        let mut netd = stack(&clock, NETD);
        let mut peer = stack(&clock, PEER);
        let listener = {
            let api = peer.api();
            let mut tcp = api.tcp::<Ipv4>();
            let id = tcp.create(SocketExtra::new());
            tcp.bind(&id, None, NonZeroU16::new(PORT)).expect("the peer binds");
            tcp.listen(&id, NonZeroUsize::new(1).expect("one")).expect("the peer listens");
            id
        };
        let extra = SocketExtra::new();
        let shared = Arc::clone(&extra.0);
        let id = {
            let api = netd.api();
            let mut tcp = api.tcp::<Ipv4>();
            let id = tcp.create(extra);
            tcp.connect(&id, to(PEER.0), NonZeroU16::new(PORT).expect("a port")).expect("netd connects");
            id
        };
        let closed = Rc::new(RefCell::new(Vec::new()));
        let (to_client, from_client) = (ring(pipe), ring(pipe));
        let end = |ring: &Pipe, writes| NetdEnd { ring: ring.clone(), writes, closed: closed.clone() };
        let conn = PipedConnection::new(id, shared, 1, end(&to_client, true), end(&from_client, false));
        let mut world = Self {
            clock,
            born: Instant::now(),
            link: Link::Up,
            // Placeholders until the accept below.
            peer_id: listener.clone(),
            peer_shared: SocketExtra::new().0,
            peer_reads: true,
            peer_got: Vec::new(),
            lost: 0,
            conn: Some(conn),
            closing: None,
            room: true,
            to_client,
            from_client,
            closed,
            netd,
            peer,
        };
        world.exchange();
        let (peer_id, _, peer_shared) = world.peer.api().tcp::<Ipv4>().accept(&listener).expect("the peer accepts");
        world.peer.api().tcp::<Ipv4>().close(listener);
        world.peer_id = peer_id;
        world.peer_shared = peer_shared;
        assert_eq!(world.netd_state(), TcpSocketState::Established, "the stream came up");
        world
    }

    fn elapsed(&self) -> Duration {
        Duration::from_nanos(self.clock.load(Ordering::Relaxed))
    }

    fn now(&self) -> Instant {
        self.born + self.elapsed()
    }

    fn conn(&mut self) -> &mut PipedConnection<NetdEnd, NetdEnd> {
        self.conn.as_mut().expect("the client still holds its stream")
    }

    fn netd_state(&mut self) -> TcpSocketState {
        let id = self.conn.as_ref().expect("the client still holds its stream").id.clone();
        state(&mut self.netd, &id)
    }

    fn peer_state(&mut self) -> TcpSocketState {
        let id = self.peer_id.clone();
        state(&mut self.peer, &id)
    }

    /// Both stacks' frames across the link and their due timers fired, until
    /// neither has anything more to do now.
    fn exchange(&mut self) {
        for _ in 0..10_000 {
            self.netd.fire_timers();
            self.peer.fire_timers();
            let mut moved = false;
            while let Some(frame) = self.netd.bindings.pop_frame() {
                moved = true;
                let data = tcp_segment(&frame).is_some_and(|(_, carries)| carries);
                match self.link {
                    Link::Up => self.peer.receive(&frame),
                    Link::Lost => self.lost += 1,
                    Link::LoseNextData if data => {
                        self.lost += 1;
                        self.link = Link::Up;
                    }
                    Link::LoseNextData => self.peer.receive(&frame),
                }
            }
            while let Some(frame) = self.peer.bindings.pop_frame() {
                moved = true;
                if self.link != Link::Lost {
                    self.netd.receive(&frame);
                }
            }
            if self.peer_reads {
                let got = &mut self.peer_got;
                if self.peer_shared.read(|b| {
                    got.extend_from_slice(b);
                    b.len()
                }) > 0
                {
                    let id = self.peer_id.clone();
                    self.peer.api().tcp::<Ipv4>().on_receive_buffer_read(&id);
                    moved = true;
                }
            }
            if !moved {
                return;
            }
        }
        panic!("the two stacks never went quiet");
    }

    /// One pass of netd's loop over this stream.
    fn pass(&mut self) {
        self.exchange();
        let now = self.now();
        if let Some(conn) = self.conn.as_mut() {
            conn.receive(&mut self.netd);
            conn.send(&mut self.netd);
            conn.tend(now);
            if conn.client_done() {
                let conn = self.conn.take().expect("held");
                self.closing = conn.finish(&mut self.netd, self.room);
            }
        }
        self.exchange();
    }

    /// Whether the stack still holds the connection the client let go of.
    fn closing_held(&mut self) -> bool {
        let Some(closing) = &self.closing else { return false };
        let cookie = closing.cookie;
        census(&mut self.netd).contains_key(&cookie)
    }

    /// Whether netd holds nothing of the stream any more.
    fn over(&mut self) -> bool {
        self.conn.is_none() && !self.closing_held()
    }

    /// Passes a tick apart for `d`, answering when in the stream was over, if
    /// it was.
    fn run(&mut self, d: Duration) -> Option<Duration> {
        let until = self.elapsed() + d;
        while self.elapsed() < until {
            self.clock.fetch_add(u64::try_from(TICK.as_nanos()).expect("a tick"), Ordering::Relaxed);
            self.pass();
            if self.over() {
                return Some(self.elapsed());
            }
        }
        None
    }

    /// Passes until `told`, answering when; red past `limit`.
    fn run_until(&mut self, limit: Duration, told: impl Fn(&mut World) -> bool) -> Duration {
        let until = self.elapsed() + limit;
        while !told(self) {
            assert!(self.elapsed() < until, "not told within {limit:?}");
            self.clock.fetch_add(u64::try_from(TICK.as_nanos()).expect("a tick"), Ordering::Relaxed);
            self.pass();
        }
        self.elapsed()
    }

    fn client_write(&mut self, bytes: &[u8]) -> Result<usize, SyscallError> {
        let mut r = self.from_client.borrow_mut();
        if !r.reader {
            return Err(SyscallError::Gone);
        }
        let n = bytes.len().min(r.cap - r.bytes.len());
        r.bytes.extend(&bytes[..n]);
        Ok(n)
    }

    /// Everything in the client's receive pipe, and whether the pipe has
    /// ended.
    fn client_read(&mut self) -> (Vec<u8>, bool) {
        let mut r = self.to_client.borrow_mut();
        (r.bytes.drain(..).collect(), !r.writer)
    }

    fn client_leaves(&mut self) {
        self.to_client.borrow_mut().reader = false;
        self.from_client.borrow_mut().writer = false;
    }

    fn peer_send(&mut self, bytes: &[u8]) {
        let id = self.peer_id.clone();
        let took = self.peer.api().tcp::<Ipv4>().with_send_buffer(&id, |ring| ring.push(bytes));
        assert_eq!(took, Some(bytes.len()), "the peer's send ring took the bytes");
        self.peer.api().tcp::<Ipv4>().do_send(&id);
    }
}

/// The moment after the first transmission that the core gives a silent peer
/// up.
fn retransmission_limit() -> Duration {
    core_limits::GIVE_UP
}

#[test]
fn the_derived_retransmission_limit_is_rfc_6298s_doubling_capped() {
    // 0.2 s doubled ten times sums to 204.6 s; the six after that are 120 s each.
    assert_eq!(retransmission_limit(), Duration::from_millis(204_600) + Duration::from_secs(720));
}

#[test]
fn every_byte_crosses_and_the_hashes_agree() {
    let mut w = World::new(1 << 20);
    let bytes: Vec<u8> = (0..300_000u32).map(|i| (i * 7 + i / 251) as u8).collect();
    let mut sent = 0;
    while sent < bytes.len() {
        sent += w.client_write(&bytes[sent..]).expect("the send pipe takes bytes");
        w.run(TICK);
    }
    w.run(Duration::from_secs(2));
    assert_eq!(w.peer_got, bytes, "the peer got every byte, in order");
    let back: Vec<u8> = bytes.iter().rev().copied().collect();
    w.peer_send(&back[..60_000]);
    w.run(Duration::from_secs(1));
    w.peer_send(&back[60_000..120_000]);
    w.run(Duration::from_secs(1));
    let (got, ended) = w.client_read();
    assert_eq!(got, back[..120_000], "the client got every byte the peer sent");
    assert!(!ended);
}

#[test]
fn a_lost_segment_is_retransmitted_on_the_cores_timer() {
    let mut w = World::new(1 << 20);
    w.link = Link::LoseNextData;
    let bytes = vec![5u8; 1000];
    assert_eq!(w.client_write(&bytes), Ok(bytes.len()));
    w.pass();
    assert_eq!(w.lost, 1, "the premise: the segment was lost");
    assert!(w.peer_got.is_empty(), "the premise: nothing else carried it");
    let got = w.run_until(Duration::from_secs(5), |w| w.peer_got.len() == bytes.len());
    assert!(got >= Duration::from_millis(200), "retransmitted {got:?} in, before the core's least RTO");
    assert_eq!(w.peer_got, bytes);
}

#[test]
fn a_peer_reset_closes_the_send_pipe_before_the_receive_pipe_ends() {
    let mut w = World::new(1 << 20);
    w.peer_send(&[7u8; 1000]);
    w.exchange();
    let id = w.peer_id.clone();
    abort(&mut w.peer, &id);
    w.pass();
    let (got, ended) = w.client_read();
    assert_eq!(got, vec![7u8; 1000], "the peer's bytes before its reset reach the client");
    assert!(ended, "the receive pipe ends once they are in it");
    assert_eq!(
        *w.closed.borrow(),
        ["send", "receive"],
        "the send pipe closes first: std reads that order, and only that order, as a reset"
    );
}

#[test]
fn a_peer_fin_ends_the_receive_pipe_after_every_byte_and_the_client_still_sends() {
    let mut w = World::new(1 << 20);
    w.peer_send(&[3u8; 5000]);
    let id = w.peer_id.clone();
    w.peer.api().tcp::<Ipv4>().shutdown(&id, ShutdownType::Send).expect("the peer shuts its half");
    w.run(Duration::from_secs(1));
    let (got, ended) = w.client_read();
    assert_eq!(got, vec![3u8; 5000]);
    assert!(ended, "EOF after the peer's last byte");
    assert_eq!(*w.closed.borrow(), ["receive"], "a FIN leaves the send pipe open");
    assert_eq!(w.client_write(&[9u8; 700]), Ok(700));
    w.run(Duration::from_secs(1));
    assert_eq!(w.peer_got, vec![9u8; 700], "the half still open carries the client's bytes");
}

#[test]
fn a_reader_that_leaves_with_the_peer_still_sending_resets_the_peer() {
    let mut w = World::new(1 << 20);
    w.to_client.borrow_mut().reader = false;
    w.peer_send(&[1u8; 2000]);
    w.run(Duration::from_secs(1));
    assert_eq!(w.peer_state(), TcpSocketState::Close, "the peer is told at once, by a reset (RFC 2525 §2.17)");
}

/// Pass until the client can tell its stream is over, which has to be the
/// core's retransmission limit after the link was lost.
fn told_at_the_retransmission_limit(w: &mut World, told: impl Fn(&mut World) -> bool) {
    let gone = w.elapsed();
    let at = w.run_until(retransmission_limit() * 2, told);
    let limit = gone + retransmission_limit();
    assert!(
        at >= limit - TICK && at <= limit + Duration::from_secs(1),
        "told {at:?} in; its peer went at {gone:?}, and the core's limit is {:?}",
        retransmission_limit()
    );
}

#[test]
fn a_stream_whose_peer_has_gone_is_reset_at_the_cores_retransmission_limit() {
    let mut w = World::new(1 << 20);
    w.link = Link::Lost;
    assert_eq!(w.client_write(&[1u8; 4096]), Ok(4096));
    told_at_the_retransmission_limit(&mut w, |w| w.client_write(&[]) == Err(SyscallError::Gone));
    assert!(w.client_read().1, "and its receive pipe ends, behind the send pipe: a reset");
    assert_eq!(*w.closed.borrow(), ["send", "receive"]);
}

#[test]
fn a_fin_its_peer_never_acknowledges_ends_the_stream_at_the_cores_retransmission_limit() {
    let mut w = World::new(1 << 20);
    w.link = Link::Lost;
    w.conn().fin_after_drain = true;
    told_at_the_retransmission_limit(&mut w, |w| w.client_read().1);
    assert_eq!(*w.closed.borrow(), ["send", "receive"], "as a reset");
}

/// **The core's own policy, not Linux's**: a peer that answers every
/// zero-window probe with its window still shut is given up at the same limit
/// as a silent one — the core counts a probe as a retransmission and the
/// peer's ACK of nothing as no progress (its `user_timeout` test,
/// `zwp_max_retries`). The client learns it as a reset.
#[test]
fn a_clients_stream_its_peer_holds_at_a_shut_window_is_reset_at_the_cores_retransmission_limit() {
    let mut w = World::new(1 << 20);
    w.peer_reads = false;
    let bytes = vec![1u8; 256 * 1024];
    assert_eq!(w.client_write(&bytes), Ok(bytes.len()));
    w.run(Duration::from_secs(1));
    let at = w.run_until(retransmission_limit() * 2, |w| w.client_write(&[]) == Err(SyscallError::Gone));
    assert!(
        at >= retransmission_limit() - Duration::from_secs(1) && at <= retransmission_limit() + Duration::from_secs(2),
        "reset {at:?} in; the core's limit is {:?}",
        retransmission_limit()
    );
    assert_eq!(*w.closed.borrow(), ["send", "receive"], "as a reset");
}

/// An orphan still holding bytes in its client's send pipe is netd's until the
/// pipe is empty: they move into the connection as its peer's window opens,
/// and the FIN follows the last of them.
#[test]
fn an_orphan_owing_bytes_delivers_every_one_once_its_peer_reads() {
    let mut w = World::new(1 << 20);
    w.peer_reads = false;
    let bytes = vec![1u8; 256 * 1024];
    assert_eq!(w.client_write(&bytes), Ok(bytes.len()));
    w.run(Duration::from_secs(1));
    w.client_leaves();
    assert_eq!(w.run(Duration::from_secs(300)), None, "a peer holding its window shut is waited for");
    assert!(w.conn().orphaned_at().is_some(), "the premise: an orphan, its send pipe not yet drained");
    w.peer_reads = true;
    w.run(Duration::from_secs(5));
    assert_eq!(w.peer_got.len(), bytes.len(), "the orphan delivered every byte");
    assert!(w.conn.is_none() && w.closing.is_some(), "and once its pipe drained, the stack finishes it");
    assert_eq!(w.peer_state(), TcpSocketState::CloseWait, "its FIN after the last byte");
}

#[test]
fn an_orphan_in_fin_wait_2_is_let_go_at_the_cores_timeout() {
    let mut w = World::new(1 << 20);
    w.client_leaves();
    w.run(Duration::from_secs(1));
    assert!(w.closing_held(), "the premise: the stack holds the orphan");
    let acked = w.elapsed();
    let over = w.run(Duration::from_secs(120)).expect("let go of");
    let timeout = core_limits::FIN_WAIT_2;
    assert!(
        over >= acked + timeout - Duration::from_secs(1) && over <= acked + timeout + Duration::from_secs(1),
        "let go of {over:?} in, its FIN acknowledged by {acked:?}; FIN-WAIT-2's timeout is {timeout:?}"
    );
    assert_eq!(w.peer_state(), TcpSocketState::CloseWait, "the peer never sent its FIN");
}

#[test]
fn a_stream_its_client_closed_first_leaves_time_wait_after_two_msl() {
    let mut w = World::new(1 << 20);
    w.client_leaves();
    w.run(Duration::from_secs(1));
    let id = w.peer_id.clone();
    w.peer.api().tcp::<Ipv4>().shutdown(&id, ShutdownType::Send).expect("the peer shuts its half");
    w.run(Duration::from_secs(1));
    assert!(w.closing_held(), "the premise: the stack holds it in TIME-WAIT");
    let closed = w.elapsed();
    let over = w.run(Duration::from_secs(300)).expect("let go of");
    let two_msl = core_limits::TIME_WAIT;
    assert!(
        over >= closed + two_msl - Duration::from_secs(2) && over <= closed + two_msl + Duration::from_secs(1),
        "let go of {over:?} in, closed at {closed:?}"
    );
}

#[test]
fn an_orphan_the_closing_table_has_no_room_for_is_reset_and_gone_at_once() {
    let mut w = World::new(1 << 20);
    w.peer_reads = false;
    // The peer's receive buffer takes 64 KiB and the send ring the rest, so
    // the pipe drains and the orphan owes its peer 32 KiB when it finishes.
    assert_eq!(w.client_write(&[1u8; 96 * 1024]), Ok(96 * 1024));
    w.run(Duration::from_secs(1));
    w.room = false;
    w.client_leaves();
    w.run(TICK);
    assert!(w.conn.is_none(), "the premise: its client is done with it");
    assert!(w.closing.is_none(), "netd keeps nothing of it");
    assert!(census(&mut w.netd).is_empty(), "and neither does the stack");
    // The core's RST carries SND.NXT, one past the byte its last zero-window
    // probe sent, which a peer at a shut window drops (RFC 9293 §3.10.7.4);
    // the peer learns at its next segment, which this stack answers with a
    // reset of its own (issues/network/netstack3-resets-past-a-shut-window.md).
    w.run(Duration::from_secs(5));
    assert_eq!(w.peer_state(), TcpSocketState::Established, "the premise: the first RST was dropped");
    w.peer_reads = true;
    w.run(Duration::from_secs(1));
    assert_eq!(w.peer_state(), TcpSocketState::Close, "the peer it owed is told by a reset once it speaks");
}

#[test]
fn an_orphan_that_owes_nothing_and_finds_no_room_is_gone_at_once() {
    let mut w = World::new(1 << 20);
    w.room = false;
    w.client_leaves();
    w.run(Duration::from_secs(1));
    assert!(w.conn.is_none() && w.closing.is_none());
    assert!(census(&mut w.netd).is_empty(), "the stack holds nothing of it");
}

/// The first SYN a fresh stack at `nanos` sends from `port` to the peer.
fn first_isn(nanos: u64, port: u16) -> u32 {
    let clock = Arc::new(AtomicU64::new(nanos));
    let mut netd = stack(&clock, NETD);
    let api = netd.api();
    let mut tcp = api.tcp::<Ipv4>();
    let id = tcp.create(SocketExtra::new());
    tcp.bind(&id, None, NonZeroU16::new(port)).expect("netd binds");
    tcp.connect(&id, to(PEER.0), NonZeroU16::new(PORT).expect("a port")).expect("netd connects");
    // ARP first: answer it as the peer would, and the SYN follows.
    let mut peer = stack(&clock, PEER);
    for _ in 0..4 {
        while let Some(frame) = netd.bindings.pop_frame() {
            if let Some((seq, _)) = tcp_segment(&frame) {
                return seq;
            }
            peer.receive(&frame);
        }
        while let Some(frame) = peer.bindings.pop_frame() {
            netd.receive(&frame);
        }
    }
    panic!("netd sent no SYN");
}

/// **The initial sequence number rests on the bindings' random source**:
/// two stacks at the same instant connecting over the same four-tuple choose
/// different ones (RFC 6528 §3, whose secret key the core draws from it).
#[test]
fn two_stacks_choose_different_initial_sequence_numbers_for_one_connection() {
    let isns: Vec<u32> = (0..4).map(|_| first_isn(1_000_000_000, 40_000)).collect();
    for (i, a) in isns.iter().enumerate() {
        for b in &isns[i + 1..] {
            assert_ne!(a, b, "two stacks chose one ISN: {isns:08x?}");
        }
    }
}
