//! MEASUREMENT ONLY, never lands: the connection-setup scout's stamps, every one
//! nanoseconds since boot on the clock page's clock, the clock the job's own
//! stamps and the claim's interrupt record are on.
//!
//! Per request: `n1` the wait that accepted the client's connection returned,
//! `n2` its request was read whole, `n6` its answer was about to be written:
//! the write can hand the processor to the client it wakes. Per lookup and
//! per connect, from the frames themselves: `n3` its query or SYN was handed to
//! the card, `n4` the first interrupt message of the pass that received the
//! reply landed (the claim's record), `n5` the reply was handed to the node.
//! `n4` is the reply's own interrupt only where the reply was its batch's first
//! frame (`idx=0`) and the message landed after the last pass found the ring
//! empty (`prev`, `u64::MAX` for a pass that stopped at its budget): a message
//! before that may be a frame's that pass consumed.
//! Per connection, its first data: `n3` the last segment this end sent before
//! the peer acknowledged any, `n4`/`n5` the segment that first did, and its
//! payload's length. Each is said as one `netstack: stamp <kind>` line.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

use toyos::say;

pub fn now() -> u64 {
    toyos_abi::clock::nanos_since_boot()
}

/// The claim's record as the last pass's begin took it: when its first message
/// landed, and how many it held (0 for a pass no interrupt began).
static IRQ_FIRST: AtomicU64 = AtomicU64::new(0);
static IRQ_COUNT: AtomicU32 = AtomicU32::new(0);

pub fn irq(record: Option<&toyos_abi::pci::DeviceIrqRecord>) {
    IRQ_FIRST.store(record.map_or(0, |r| r.first_nanos), Relaxed);
    IRQ_COUNT.store(record.map_or(0, |r| r.count), Relaxed);
}

type Remote = ([u8; 4], u16);

/// A reply: `n4`, the record's count, `n5`, its place in its batch, and when
/// the pass before last found the ring empty.
#[derive(Clone, Copy, Default)]
struct Heard {
    n4: u64,
    irqs: u32,
    n5: u64,
    idx: u32,
    prev: u64,
}

impl Heard {
    fn words(&self) -> String {
        format!("n4={} irqs={} n5={} idx={} prev={}", self.n4, self.irqs, self.n5, self.idx, self.prev)
    }
}

struct Syn {
    local: u16,
    n3: u64,
    sent: u32,
    reply: Option<(Heard, &'static str)>,
}

struct Query {
    n3: u64,
    sent: u32,
    reply: Option<Heard>,
}

struct Data {
    n3: u64,
    segs: u32,
    bytes: u32,
    end: u32,
}

#[derive(Default)]
struct State {
    irq_first: u64,
    irqs: u32,
    /// When the last pass found the ring empty, and when this one did.
    prev: u64,
    emptied: Option<u64>,
    /// The frame's place in this pass's receive batch.
    idx: u32,
    syn: HashMap<Remote, Syn>,
    /// By the query's own port.
    dns: HashMap<u16, Query>,
    /// The ports of the replies this pass received, in order.
    replied: Vec<u16>,
    /// Every query's port and when it left, the newest last.
    asked: std::collections::VecDeque<(u16, u64)>,
    /// Connections whose first data is watched, by their peer and own port.
    data: HashMap<(Remote, u16), Option<Data>>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// Bounds each table: an entry nothing consumes goes with the rest.
const KEPT: usize = 64;

/// A pass began: the record its begin took.
pub fn pass() {
    STATE.with_borrow_mut(|s| {
        s.irq_first = IRQ_FIRST.load(Relaxed);
        s.irqs = IRQ_COUNT.load(Relaxed);
        s.prev = s.emptied.take().unwrap_or(u64::MAX);
        s.idx = 0;
        s.replied.clear();
    });
}

/// The pass's receive found the ring empty.
pub fn emptied() {
    let at = now();
    STATE.with_borrow_mut(|s| s.emptied = Some(at));
}

/// What a frame is, as far as the stamps care.
enum Seen {
    Udp { src: Remote, dst: Remote },
    Tcp { src: Remote, dst: Remote, flags: u8, seq: u32, ack: u32, payload: u32 },
}

const SYN: u8 = 0x02;
const RST: u8 = 0x04;
const ACK: u8 = 0x10;

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn seen(frame: &[u8]) -> Option<Seen> {
    if be16(frame, 12)? != 0x0800 {
        return None;
    }
    let ip = 14;
    let ihl = usize::from(frame.get(ip)? & 0x0f) * 4;
    let total = usize::from(be16(frame, ip + 2)?);
    let proto = *frame.get(ip + 9)?;
    let addr = |at: usize| -> Option<[u8; 4]> { frame.get(at..at + 4)?.try_into().ok() };
    let (src_ip, dst_ip) = (addr(ip + 12)?, addr(ip + 16)?);
    let l4 = ip + ihl;
    let (sport, dport) = (be16(frame, l4)?, be16(frame, l4 + 2)?);
    let (src, dst) = ((src_ip, sport), (dst_ip, dport));
    match proto {
        17 => Some(Seen::Udp { src, dst }),
        6 => {
            let doff = usize::from(frame.get(l4 + 12)? >> 4) * 4;
            let payload = total.checked_sub(ihl + doff)? as u32;
            Some(Seen::Tcp { src, dst, flags: *frame.get(l4 + 13)?, seq: be32(frame, l4 + 4)?, ack: be32(frame, l4 + 8)?, payload })
        }
        _ => None,
    }
}

/// A frame handed to the card.
pub fn tx(frame: &[u8]) {
    let Some(seen) = seen(frame) else { return };
    let at = now();
    STATE.with_borrow_mut(|s| match seen {
        Seen::Udp { src, dst } if dst.1 == 53 => {
            if s.dns.len() >= KEPT {
                s.dns.clear();
            }
            let q = s.dns.entry(src.1).or_insert(Query { n3: at, sent: 0, reply: None });
            q.sent += 1;
            if s.asked.len() >= KEPT {
                s.asked.pop_front();
            }
            s.asked.push_back((src.1, at));
        }
        Seen::Udp { .. } => {}
        Seen::Tcp { src, dst, flags, seq, payload, .. } => {
            if flags & SYN != 0 && flags & ACK == 0 {
                if s.syn.len() >= KEPT {
                    s.syn.clear();
                }
                match s.syn.get_mut(&dst) {
                    Some(syn) if syn.local == src.1 => syn.sent += 1,
                    _ => {
                        s.syn.insert(dst, Syn { local: src.1, n3: at, sent: 1, reply: None });
                    }
                }
            } else if payload > 0 {
                if let Some(watch) = s.data.get_mut(&(dst, src.1)) {
                    let d = watch.get_or_insert(Data { n3: at, segs: 0, bytes: 0, end: seq });
                    d.n3 = at;
                    d.segs += 1;
                    d.bytes += payload;
                    d.end = seq.wrapping_add(payload);
                }
            }
        }
    });
}

/// A received frame, about to be handed to the node.
pub fn rx(frame: &[u8]) {
    let seen = seen(frame);
    let at = now();
    STATE.with_borrow_mut(|s| {
        let heard = Heard { n4: s.irq_first, irqs: s.irqs, n5: at, idx: s.idx, prev: s.prev };
        s.idx += 1;
        match seen {
            Some(Seen::Udp { src, dst }) if src.1 == 53 => {
                if let Some(q) = s.dns.get_mut(&dst.1) {
                    if q.reply.is_none() {
                        q.reply = Some(heard);
                        s.replied.push(dst.1);
                    }
                }
            }
            Some(Seen::Tcp { src, dst, flags, ack, payload, .. }) => {
                if let Some(syn) = s.syn.get_mut(&src) {
                    if syn.local == dst.1 && syn.reply.is_none() {
                        if flags & RST != 0 {
                            syn.reply = Some((heard, "rst"));
                        } else if flags & (SYN | ACK) == SYN | ACK {
                            syn.reply = Some((heard, "synack"));
                        }
                    }
                }
                let key = (src, dst.1);
                let acked = match s.data.get(&key) {
                    Some(Some(d)) if flags & ACK != 0 && ack.wrapping_sub(d.end) as i32 >= 0 => Some((d.n3, d.segs, d.bytes)),
                    _ => None,
                };
                if let Some((n3, segs, bytes)) = acked {
                    say!("netstack: stamp data n3={n3} {} segs={segs} bytes={bytes} ack_len={payload}", heard.words());
                    s.data.remove(&key);
                }
            }
            _ => {}
        }
    });
}

/// What a request carried in: when the wait that accepted its connection
/// returned, and when it was read whole.
#[derive(Clone, Copy, Default)]
pub struct Asked {
    pub n1: u64,
    pub n2: u64,
}

/// A request answered at once, its answer about to go at `n6`.
pub fn answered(kind: &str, asked: Asked, n6: u64) {
    say!("netstack: stamp {kind} n1={} n2={} n6={n6}", asked.n1, asked.n2);
}

/// The lookups this settle answered, each with what it was asked: the replies
/// this pass received are theirs, in order.
pub fn lookups(done: &[(Asked, u64)]) {
    if done.is_empty() {
        return;
    }
    STATE.with_borrow_mut(|s| {
        let replied = std::mem::take(&mut s.replied);
        let clean = replied.len() == done.len();
        for (k, (asked, n6)) in done.iter().enumerate() {
            let q = replied.get(k).and_then(|port| s.dns.remove(port));
            let (n3, sent, heard) = q.map_or((0, 0, Heard::default()), |q| (q.n3, q.sent, q.reply.unwrap_or_default()));
            // Every query that left while this lookup was asked: more than one is an alias
            // chased, a resolver asked again, or another lookup's.
            let since: Vec<u64> = s.asked.iter().map(|(_, at)| *at).filter(|at| (asked.n2..=*n6).contains(at)).collect();
            say!(
                "netstack: stamp lookup n1={} n2={} n3={n3} {} n6={n6} sent={sent} queries_between={} first_query={} replies={}/{} clean={}",
                asked.n1,
                asked.n2,
                heard.words(),
                since.len(),
                since.first().copied().unwrap_or(0),
                replied.len(),
                done.len(),
                u8::from(clean)
            );
        }
    });
}

/// A connect to `remote` answered at `n6`, from `local` where it connected.
pub fn connected(remote: Remote, local: Option<u16>, asked: Asked, n6: u64, outcome: &str) {
    STATE.with_borrow_mut(|s| {
        let syn = s.syn.remove(&remote);
        let (n3, sent, heard, reply) = match &syn {
            Some(syn) => match syn.reply {
                Some((heard, kind)) => (syn.n3, syn.sent, heard, kind),
                None => (syn.n3, syn.sent, Heard::default(), "none"),
            },
            None => (0, 0, Heard::default(), "none"),
        };
        say!(
            "netstack: stamp connect n1={} n2={} n3={n3} {} n6={n6} syns={sent} reply={reply} outcome={outcome}",
            asked.n1,
            asked.n2,
            heard.words()
        );
        if let Some(local) = local {
            if s.data.len() >= KEPT {
                s.data.clear();
            }
            s.data.insert((remote, local), None);
        }
    });
}
