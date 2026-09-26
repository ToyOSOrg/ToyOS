//! A stream's life on a wire: netd's socket on smoltcp's own `Interface`, its
//! peer a second smoltcp stack on the far end of a link these tests can cut,
//! and the client's two pipes played here, so what is judged is what the
//! client and the peer each see.

use super::*;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use smoltcp::iface::{Config, Interface, PollResult, SocketSet};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{
    EthernetAddress, EthernetFrame, EthernetProtocol, HardwareAddress, IpAddress, IpCidr, IpEndpoint, IpProtocol,
    Ipv4Address, Ipv4Packet, TcpPacket,
};

const NETD: (Ipv4Address, EthernetAddress) = (Ipv4Address::new(10, 0, 0, 2), EthernetAddress([2, 0, 0, 0, 0, 2]));
const PEER: (Ipv4Address, EthernetAddress) = (Ipv4Address::new(10, 0, 0, 3), EthernetAddress([2, 0, 0, 0, 0, 3]));
const PORT: u16 = 80;

/// What the link does with a frame.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Link {
    Up,
    /// Every frame either way is lost: a peer that has gone.
    Lost,
    /// No frame can leave either stack: a neighbour that no longer answers,
    /// so nothing addressed to it is ever sent.
    Refused,
    /// The peer refuses netd's FIN, as RFC 9293's acceptability test refuses
    /// one at a shut window, and answers it with an ACK: the FIN reaches it
    /// as a segment one before its window, which it answers the same way.
    RefusesFins,
}

/// [`Link::RefusesFins`]' rewrite of a frame of netd's carrying a FIN and no
/// bytes, and of nothing else.
fn refuse_fin(frame: &mut [u8]) {
    let mut ethernet = EthernetFrame::new_unchecked(frame);
    if ethernet.ethertype() != EthernetProtocol::Ipv4 {
        return;
    }
    let mut ip = Ipv4Packet::new_unchecked(ethernet.payload_mut());
    let (src, dst) = (IpAddress::Ipv4(ip.src_addr()), IpAddress::Ipv4(ip.dst_addr()));
    let carries_tcp = ip.next_header() == IpProtocol::Tcp;
    let mut tcp = TcpPacket::new_unchecked(ip.payload_mut());
    if carries_tcp && tcp.fin() && tcp.payload_mut().is_empty() {
        tcp.set_fin(false);
        tcp.set_seq_number(tcp.seq_number() - 1);
        tcp.fill_checksum(&src, &dst);
    }
}

#[derive(Default)]
struct Queues {
    to: [VecDeque<Vec<u8>>; 2],
}

struct Side {
    queues: Rc<RefCell<Queues>>,
    link: Rc<RefCell<Link>>,
    me: usize,
}

struct Rx(Vec<u8>);

struct Tx {
    queues: Rc<RefCell<Queues>>,
    link: Link,
    to: usize,
}

impl phy::RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for Tx {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut frame = vec![0u8; len];
        let result = f(&mut frame);
        if self.link == Link::RefusesFins && self.to == 1 {
            refuse_fin(&mut frame);
        }
        if matches!(self.link, Link::Up | Link::RefusesFins) {
            self.queues.borrow_mut().to[self.to].push_back(frame);
        }
        result
    }
}

impl Device for Side {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx;

    fn receive(&mut self, _: SmolInstant) -> Option<(Rx, Tx)> {
        let frame = self.queues.borrow_mut().to[self.me].pop_front()?;
        Some((Rx(frame), self.tx()?))
    }

    fn transmit(&mut self, _: SmolInstant) -> Option<Tx> {
        self.tx()
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1514;
        caps.medium = Medium::Ethernet;
        caps
    }
}

impl Side {
    fn tx(&self) -> Option<Tx> {
        let link = *self.link.borrow();
        (link != Link::Refused).then(|| Tx { queues: self.queues.clone(), link, to: 1 - self.me })
    }
}

/// One direction of a client's pipe pair, as the kernel's ring behaves.
struct Ring {
    bytes: VecDeque<u8>,
    cap: usize,
    writer: bool,
    reader: bool,
}

type Shared = Rc<RefCell<Ring>>;

fn ring(cap: usize) -> Shared {
    Rc::new(RefCell::new(Ring { bytes: VecDeque::new(), cap, writer: true, reader: true }))
}

/// netd's end of one of the pair, which writes (the receive pipe) or reads
/// (the send pipe), and says in `closed` when it is dropped.
struct NetdEnd {
    ring: Shared,
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

/// netd's socket, its peer's, the link between them, and the client.
struct World {
    born: Instant,
    now: Instant,
    queues: Rc<RefCell<Queues>>,
    link: Rc<RefCell<Link>>,
    netd: (Interface, Side, SocketSet<'static>),
    peer: (Interface, Side, SocketSet<'static>),
    peer_socket: smoltcp::iface::SocketHandle,
    /// Whether the peer reads what arrives, which is what opens its window.
    peer_reads: bool,
    peer_got: Vec<u8>,
    conn: PipedConnection<NetdEnd, NetdEnd>,
    to_client: Shared,
    from_client: Shared,
    /// netd's ends, in the order netd closed them.
    closed: Rc<RefCell<Vec<&'static str>>>,
}

fn stack(queues: &Rc<RefCell<Queues>>, link: &Rc<RefCell<Link>>, me: usize) -> (Interface, Side, SocketSet<'static>) {
    let (ip, mac) = [NETD, PEER][me];
    let mut side = Side { queues: queues.clone(), link: link.clone(), me };
    let mut iface = Interface::new(Config::new(HardwareAddress::Ethernet(mac)), &mut side, SmolInstant::from_millis(0));
    iface.update_ip_addrs(|addrs| addrs.push(IpCidr::new(IpAddress::Ipv4(ip), 24)).unwrap());
    (iface, side, SocketSet::new(Vec::new()))
}

fn socket() -> tcp::Socket<'static> {
    tcp::Socket::new(tcp::SocketBuffer::new(vec![0u8; 65536]), tcp::SocketBuffer::new(vec![0u8; 65536]))
}

impl World {
    /// A stream established to a listening peer, its client holding both
    /// ends of a pair of pipes `pipe` bytes deep.
    fn new(pipe: usize) -> Self {
        let queues = Rc::new(RefCell::new(Queues::default()));
        let link = Rc::new(RefCell::new(Link::Up));
        let mut netd = stack(&queues, &link, 0);
        let mut peer = stack(&queues, &link, 1);
        let mut listener = socket();
        listener.listen(PORT).unwrap();
        let peer_socket = peer.2.add(listener);
        let mut client = socket();
        client.connect(netd.0.context(), IpEndpoint::new(IpAddress::Ipv4(PEER.0), PORT), 49152).unwrap();
        let handle = netd.2.add(client);
        let born = Instant::now();
        let closed = Rc::new(RefCell::new(Vec::new()));
        let (to_client, from_client) = (ring(pipe), ring(pipe));
        let end = |ring: &Shared, writes| NetdEnd { ring: ring.clone(), writes, closed: closed.clone() };
        let conn = PipedConnection::new(handle, 1, end(&to_client, true), end(&from_client, false), born);
        let mut world = Self {
            born,
            now: born,
            queues,
            link,
            netd,
            peer,
            peer_socket,
            peer_reads: true,
            peer_got: Vec::new(),
            conn,
            to_client,
            from_client,
            closed,
        };
        world.poll();
        assert_eq!(world.netd_socket().state(), tcp::State::Established, "the stream came up");
        world
    }

    fn at(&self) -> SmolInstant {
        SmolInstant::from_millis((self.now - self.born).as_millis() as i64)
    }

    fn netd_socket(&mut self) -> &mut tcp::Socket<'static> {
        self.netd.2.get_mut::<tcp::Socket>(self.conn.handle)
    }

    fn peer_socket(&mut self) -> &mut tcp::Socket<'static> {
        self.peer.2.get_mut::<tcp::Socket>(self.peer_socket)
    }

    /// Both stacks, until neither has anything more to do now.
    fn poll(&mut self) {
        let at = self.at();
        for _ in 0..10_000 {
            let a = self.netd.0.poll(at, &mut self.netd.1, &mut self.netd.2);
            if self.peer_reads {
                let got = &mut self.peer_got;
                let socket = self.peer.2.get_mut::<tcp::Socket>(self.peer_socket);
                while socket.can_recv() {
                    socket.recv(|b| (b.len(), got.extend_from_slice(b))).unwrap();
                }
            }
            let b = self.peer.0.poll(at, &mut self.peer.1, &mut self.peer.2);
            let quiet = self.queues.borrow().to.iter().all(VecDeque::is_empty);
            if a == PollResult::None && b == PollResult::None && quiet {
                return;
            }
        }
        panic!("the two stacks never went quiet");
    }

    /// One pass of netd's loop over this stream, and what it decided.
    fn pass(&mut self) -> Fate {
        self.poll();
        let now = self.now;
        let socket = self.netd.2.get_mut::<tcp::Socket>(self.conn.handle);
        self.conn.receive(socket, now);
        self.conn.send(socket, now);
        let fate = self.conn.tend(socket, now);
        self.poll();
        fate
    }

    /// Passes every 100 ms for `d`, answering how long in the stream was over,
    /// if it was.
    fn run(&mut self, d: Duration) -> Option<Duration> {
        let until = self.now + d;
        while self.now < until {
            self.now += Duration::from_millis(100);
            if self.pass() == Fate::Over {
                return Some(self.now - self.born);
            }
        }
        None
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

    fn since_born(&self) -> Duration {
        self.now - self.born
    }
}

#[test]
fn a_peer_reset_closes_the_send_pipe_before_the_receive_pipe_ends() {
    let mut w = World::new(1 << 20);
    w.peer_socket().send_slice(&[7u8; 1000]).unwrap();
    w.poll();
    w.peer_socket().abort();
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
fn a_reset_whose_rst_cannot_leave_is_let_go_of_once_its_linger_passes() {
    let mut w = World::new(1 << 20);
    *w.link.borrow_mut() = Link::Refused;
    w.client_leaves();
    let left = w.since_born();
    let over = w.run(STALL_LIMIT + RST_LINGER + Duration::from_secs(10)).expect("the stream is let go of");
    // Reset for its FIN going unacknowledged, then given up on at its linger.
    let want = left + STALL_LIMIT + RST_LINGER;
    assert!(
        over >= want && over <= want + Duration::from_secs(1),
        "let go of {over:?} in, and {want:?} is the stall limit and then the linger"
    );
    assert!(!super::finished(w.netd_socket()), "the premise: its RST never left");
}

#[test]
fn an_orphans_fin_wait_2_limit_runs_from_its_fin_being_acknowledged() {
    let mut w = World::new(1 << 20);
    w.peer_reads = false;
    let bytes = vec![1u8; 256 * 1024];
    assert_eq!(w.client_write(&bytes), Ok(bytes.len()));
    w.run(Duration::from_secs(1));
    w.client_leaves();
    assert_eq!(w.run(Duration::from_secs(50)), None, "a peer holding its window shut 50 s is waited for");
    w.peer_reads = true;
    let acked = w.since_born();
    assert_eq!(w.run(Duration::from_secs(5)), None);
    assert_eq!(w.netd_socket().state(), tcp::State::FinWait2, "the premise: every byte and the FIN arrived");
    assert_eq!(w.peer_got.len(), bytes.len(), "the orphan delivered every byte");
    assert_eq!(w.run(FIN_WAIT_2_LIMIT - Duration::from_secs(6)), None, "reset before its limit");
    let over = w.run(Duration::from_secs(3)).expect("reset once its peer's FIN is overdue");
    assert!(over >= acked + FIN_WAIT_2_LIMIT, "reset {over:?} in, before {acked:?} + the limit");
}

#[test]
fn an_orphan_its_peer_holds_at_a_shut_window_is_reset_once_nothing_has_moved_for_the_stall_limit() {
    let mut w = World::new(1 << 20);
    w.peer_reads = false;
    let bytes = vec![1u8; 256 * 1024];
    assert_eq!(w.client_write(&bytes), Ok(bytes.len()));
    w.run(Duration::from_secs(1));
    w.client_leaves();
    let left = w.since_born();
    let over = w.run(STALL_LIMIT + Duration::from_secs(10)).expect("an orphan held shut is reset");
    assert!(
        over >= left + STALL_LIMIT && over <= left + STALL_LIMIT + Duration::from_secs(1),
        "reset {over:?} in; its client left at {left:?}"
    );
}

#[test]
fn a_clients_stream_at_a_shut_window_its_peer_answers_is_never_reset() {
    let mut w = World::new(1 << 20);
    w.peer_reads = false;
    let bytes = vec![1u8; 256 * 1024];
    assert_eq!(w.client_write(&bytes), Ok(bytes.len()));
    assert_eq!(w.run(STALL_LIMIT * 6), None);
    assert_eq!(w.netd_socket().state(), tcp::State::Established);
    assert_eq!(w.client_write(&[1]), Ok(1), "its client can still write");
}

/// Pass until `told` says the client knows its stream is over, which has to
/// be the stall limit after the link was lost.
fn told_at_the_stall_limit(w: &mut World, told: impl Fn(&mut World) -> bool) {
    let gone = w.since_born();
    while !told(w) {
        assert!(w.since_born() < gone + STALL_LIMIT * 2, "the client is never told its stream is over");
        w.run(Duration::from_millis(100));
    }
    let at = w.since_born();
    let limit = gone + STALL_LIMIT;
    assert!(at >= limit && at <= limit + Duration::from_secs(1), "told {at:?} in; its peer went at {gone:?}");
}

#[test]
fn a_stream_whose_peer_has_gone_is_reset_after_the_stall_limit() {
    let mut w = World::new(1 << 20);
    *w.link.borrow_mut() = Link::Lost;
    assert_eq!(w.client_write(&[1u8; 4096]), Ok(4096));
    told_at_the_stall_limit(&mut w, |w| w.client_write(&[]) == Err(SyscallError::Gone));
    assert!(w.client_read().1, "and its receive pipe ends, behind the send pipe: a reset");
}

#[test]
fn a_fin_its_peer_never_acknowledges_ends_the_stream_after_the_stall_limit() {
    let mut w = World::new(1 << 20);
    *w.link.borrow_mut() = Link::Lost;
    w.conn.fin_after_drain = true;
    told_at_the_stall_limit(&mut w, |w| w.client_read().1);
    assert_eq!(*w.closed.borrow(), ["send", "receive"], "as a reset");
}

#[test]
fn a_clients_fin_its_peer_refuses_and_answers_is_never_reset() {
    let mut w = World::new(1 << 20);
    *w.link.borrow_mut() = Link::RefusesFins;
    w.conn.fin_after_drain = true;
    assert_eq!(w.run(STALL_LIMIT * 6), None);
    assert_eq!(w.netd_socket().state(), tcp::State::FinWait1, "its FIN is still owed");
    assert!(!w.client_read().1, "and its client told nothing");
}

#[test]
fn a_given_up_stream_that_owes_its_peer_is_reset_and_one_that_owes_nothing_is_over_at_once() {
    let mut w = World::new(1 << 20);
    w.peer_reads = false;
    assert_eq!(w.client_write(&[1u8; 256 * 1024]), Ok(256 * 1024));
    w.run(Duration::from_secs(1));
    w.client_leaves();
    w.pass();
    let (now, handle) = (w.now, w.conn.handle);
    assert_eq!(w.conn.give_up(w.netd.2.get_mut::<tcp::Socket>(handle), now), Fate::Open);
    w.poll();
    assert_eq!(w.peer_socket().state(), tcp::State::Closed, "the peer it owed is told by a reset");
    let over = w.run(RST_LINGER + Duration::from_secs(1)).expect("let go of once its RST has left");
    assert!(over <= now - w.born + RST_LINGER, "let go of {over:?} in");

    let mut w = World::new(1 << 20);
    w.client_leaves();
    w.run(Duration::from_secs(1));
    assert_eq!(w.netd_socket().state(), tcp::State::FinWait2, "the premise: it owes nothing");
    let (now, handle) = (w.now, w.conn.handle);
    assert_eq!(w.conn.give_up(w.netd.2.get_mut::<tcp::Socket>(handle), now), Fate::Over);
}

#[test]
fn a_closing_table_past_its_bound_gives_up_the_newest() {
    let t = Instant::now();
    let s = Duration::from_secs;
    let closing = [(0, t + s(1)), (1, t + s(5)), (2, t + s(3)), (3, t + s(4))];
    assert_eq!(past_bound(closing.into_iter(), 2), [1, 3]);
    assert_eq!(past_bound(closing.into_iter(), 4), [0usize; 0]);
}
