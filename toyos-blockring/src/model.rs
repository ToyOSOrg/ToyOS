//! Every ordering of a client, a server, a device and their failures.
//!
//! A scripted caller asks for writes and flushes; the rings between the client
//! and the server are queues, and rings of the transport's own are driven
//! beside them and held to them both ways ([`Queues`]); the server is [`ServerSession`]
//! over a device of two blocks with a volatile cache. [`explore`] runs, depth
//! first and exhaustively, every interleaving of: the caller asking for its
//! next step, the server taking a request, the device completing any one it
//! holds, **the device failing any one it holds** (not done, answered so), the
//! client reading a completion, **the device being reset** under whatever is in
//! flight (each command dropped, or run before the stop with its completion
//! read or not; the cache kept or dropped), **the server dying** (each command
//! dropped or applied; the cache kept or dropped; the rings left as they
//! were), the client noticing, and the client reconnecting to a fresh server.
//! Each of the three failures is bounded by its own count ([`Failures`]).
//! The client and the server are this crate's own types, not
//! transliterations: what is checked is the code blockd and its clients run.
//!
//! The laws:
//! - every ticket is answered exactly once — never twice, and by the end never
//!   not at all — and a server that keeps the protocol never makes the client
//!   meet a second completion for one tag ([`Law::Answers`]);
//! - a flush answered durable left on the medium, for every block, the last
//!   write acknowledged before the flush was asked for, or one asked for after
//!   it; and a flush ends answered durable unless the run spent more failures
//!   than [`MAX_ATTEMPTS`], which is the fewest that can make the client give
//!   a write or a flush up ([`Law::Durable`]).

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;
use core::sync::atomic::Ordering;
use std::collections::HashSet;

use toyos_blockhold::Holds;
use toyos_transport::{Consumer, Place, Producer, Untrusted, Word};

use crate::client::{Client, Outcome, Ticket, MAX_ATTEMPTS};
use crate::entry::{Completion, Op, Request};
use crate::layout::{ARENA, CQE_WORDS, SQE_WORDS};
use crate::server::{ServerSession, Taken};

const BLOCKS: usize = 2;

/// How many of each failure one run may meet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Failures {
    pub resets: u8,
    pub crashes: u8,
    /// Commands the device answers not done.
    pub errors: u8,
}

impl Failures {
    fn total(self) -> u32 {
        u32::from(self.resets) + u32::from(self.crashes) + u32::from(self.errors)
    }
}

/// What a run found.
pub struct Explored {
    /// The first law broken.
    pub broken: Option<(Law, String)>,
    /// End states reached.
    pub ends: usize,
    /// End states where a flush was answered the device's refusal: the client
    /// gave a write or a flush up.
    pub given_up: usize,
    /// Whether the request ring, and the completion ring, held [`DEPTH`].
    pub filled: [bool; 2],
}

/// One thing the scripted caller asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    Write { block: u64, value: u8 },
    Flush,
}

/// Which law a run broke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Law {
    Answers,
    Durable,
}

/// One word of the session page. The model runs on one thread, so every order
/// is the program's.
#[derive(Clone)]
struct Shared(Cell<u32>);

impl Word for Shared {
    fn load(&self, _: Ordering) -> u32 {
        self.0.get()
    }
    fn store(&self, value: u32, _: Ordering) {
        self.0.set(value)
    }
}

/// How deep the model's rings are: shallow enough that a script fills and
/// wraps each.
const DEPTH: u32 = 4;
const SQ_BASE: usize = 4;
const CQ_BASE: usize = SQ_BASE + DEPTH as usize * SQE_WORDS;
const WORDS: usize = CQ_BASE + DEPTH as usize * CQE_WORDS;
const SQ: Place<SQE_WORDS, DEPTH, WORDS> = Place::new::<0, 1, SQ_BASE>();
const CQ: Place<CQE_WORDS, DEPTH, WORDS> = Place::new::<2, 3, CQ_BASE>();

type ClientEnds = (Producer<SQE_WORDS, DEPTH, WORDS>, Consumer<CQE_WORDS, DEPTH, WORDS>);
type ServerEnds = (Consumer<SQE_WORDS, DEPTH, WORDS>, Producer<CQE_WORDS, DEPTH, WORDS>);

/// The rings between the client and the server, twice: as queues, the
/// reference, and as the transport's rings, which give either end exactly what
/// the queue does — every entry it holds, and nothing when it holds none. A
/// state is keyed by its queues alone ([`key`]), so the search tells apart
/// what the protocol can, not where on the page it is.
#[derive(Clone)]
struct Queues {
    sq: VecDeque<Request>,
    cq: VecDeque<Completion>,
    page: [Shared; WORDS],
    client: ClientEnds,
    server: ServerEnds,
}

impl Queues {
    fn new() -> Self {
        let page = core::array::from_fn(|_| Shared(Cell::new(0)));
        let (client, server) = Self::ends(&page);
        Self { sq: VecDeque::new(), cq: VecDeque::new(), page, client, server }
    }

    /// Both ends over `page`, every cursor stored 0.
    fn ends(page: &[Shared; WORDS]) -> (ClientEnds, ServerEnds) {
        ((Producer::new(page, SQ), Consumer::new(page, CQ)), (Consumer::new(page, SQ), Producer::new(page, CQ)))
    }

    /// The session is over: what either queue held is gone, and the next
    /// session's two ends start over the same page.
    fn reset(&mut self) {
        self.sq.clear();
        self.cq.clear();
        (self.client, self.server) = Self::ends(&self.page);
    }

    fn send(&mut self, request: Request) {
        assert_eq!(self.client.0.push(&self.page, request.encode()), Ok(true), "a request past the ring's depth");
        self.client.0.publish(&self.page);
        self.sq.push_back(request);
    }

    /// The oldest request, as the server's ring gives it.
    fn take(&mut self) -> Option<[Untrusted<u32>; SQE_WORDS]> {
        let want = self.sq.pop_front()?;
        let words = self.server.0.pop(&self.page).expect("an honest client's tail");
        let words = words.expect("the ring holds what the queue does");
        self.server.0.release(&self.page);
        assert_eq!(Request::decode(words, u64::MAX), Ok(want), "the ring gave the server what the queue holds");
        Some(words)
    }

    fn post(&mut self, c: Completion) {
        assert_eq!(self.server.1.push(&self.page, c.encode()), Ok(true), "a completion past the ring's depth");
        self.server.1.publish(&self.page);
        self.cq.push_back(c);
    }

    /// The oldest completion, as the client's ring gives it.
    fn read(&mut self) -> Option<Completion> {
        let want = self.cq.pop_front()?;
        let words: [Untrusted<u32>; CQE_WORDS] =
            self.client.1.pop(&self.page).expect("an honest server's tail").expect("the ring holds what the queue does");
        self.client.1.release(&self.page);
        assert_eq!(Completion::decode(words), Some(want), "the ring gave the client what the queue holds");
        Some(want)
    }

    /// A ring whose queue is empty gives nothing.
    fn hold_empty(&mut self) {
        if self.sq.is_empty() {
            assert_eq!(self.server.0.pop(&self.page), Ok(None), "the request ring gave what the queue does not hold");
        }
        if self.cq.is_empty() {
            assert_eq!(self.client.1.pop(&self.page), Ok(None), "the completion ring gave what the queue does not hold");
        }
    }
}

#[derive(Clone)]
struct Server {
    session: ServerSession,
    holds: Holds<u8>,
}

#[derive(Clone)]
struct World {
    client: Client<{ DEPTH as usize }>,
    next: usize,
    /// Every answer each ticket has had.
    answers: BTreeMap<Ticket, Vec<Outcome>>,
    /// For each flush ticket, the write tickets answered `Done` before it was
    /// asked for.
    acked_before: BTreeMap<Ticket, Vec<Ticket>>,
    queues: Queues,
    server: Option<Server>,
    /// The connection: false once the server has died, until a reconnect.
    alive: bool,
    losses: u64,
    /// The device's queue: `(tag, request)`.
    device: Vec<(u32, Request)>,
    /// Completions the device posted before a reset, not yet read.
    posted: Vec<u32>,
    cache: [Option<u8>; BLOCKS],
    media: [u8; BLOCKS],
    /// What is left of the run's failures.
    left: Failures,
}

struct Run<'a> {
    script: &'a [Step],
    /// Visited states, by [`key`].
    seen: HashSet<String>,
    broken: Option<(Law, String)>,
    ends: usize,
    given_up: usize,
    filled: [bool; 2],
    /// Whether a flush may end answered `Device`.
    may_give_up: bool,
    /// The steps from the start to here, for a failure to name.
    path: Vec<String>,
}

/// Explore `script` against at most `failures`.
pub fn explore(script: &[Step], failures: Failures) -> Explored {
    let mut run = Run {
        script,
        seen: HashSet::new(),
        broken: None,
        ends: 0,
        given_up: 0,
        filled: [false; 2],
        may_give_up: failures.total() > MAX_ATTEMPTS,
        path: Vec::new(),
    };
    dfs(&mut run, start(failures));
    Explored { broken: run.broken, ends: run.ends, given_up: run.given_up, filled: run.filled }
}

/// The client connected to a fresh server, with nothing asked yet.
fn start(failures: Failures) -> World {
    let mut world = World {
        client: Client::new(),
        next: 0,
        answers: BTreeMap::new(),
        acked_before: BTreeMap::new(),
        queues: Queues::new(),
        server: None,
        alive: false,
        losses: 0,
        device: Vec::new(),
        posted: Vec::new(),
        cache: [None; BLOCKS],
        media: [0; BLOCKS],
        left: failures,
    };
    connect(&mut world);
    world
}

fn connect(world: &mut World) {
    let mut holds = Holds::new();
    holds.hold(0, BLOCKS as u64, 1).expect("a fresh server holds nothing");
    world.server = Some(Server { session: ServerSession::new(0, BLOCKS as u64), holds });
    world.alive = true;
    world.losses = 0;
    world.client.session_started();
    pump(world);
}

/// Every request the client will send goes onto the ring.
fn pump(world: &mut World) {
    while let Some(request) = world.client.next_request() {
        world.queues.send(request);
    }
}

/// A write step's block and value; a write's arena block is its ticket.
fn write_of(script: &[Step], ticket: Ticket) -> Option<(u64, u8)> {
    match script[ticket as usize] {
        Step::Write { block, value } => Some((block, value)),
        Step::Flush => None,
    }
}

/// The device carries out `request`: a write lands in the cache, a flush moves
/// the cache onto the medium.
fn apply(script: &[Step], cache: &mut [Option<u8>; BLOCKS], media: &mut [u8; BLOCKS], request: Request) {
    match request.op {
        Op::Write { run, lba } => {
            let (_, value) = write_of(script, u64::from(run.first())).expect("a write's arena block is its ticket");
            cache[lba as usize] = Some(value);
        }
        Op::Flush => {
            for (b, slot) in cache.iter_mut().enumerate() {
                if let Some(v) = slot.take() {
                    media[b] = v;
                }
            }
        }
        Op::Read { .. } => {}
    }
}

/// What a reset or a death leaves behind: the completions the device had
/// posted that the server has yet to read, and the cache and medium.
type Fate = (Vec<u32>, [Option<u8>; BLOCKS], [u8; BLOCKS]);

/// Every way a reset or a death leaves the device. **Nothing runs after the
/// device is stopped**, so each command it held was dropped, or ran before the
/// stop — and, for a reset, may have posted a completion the server reads only
/// afterwards. The cache is kept or dropped.
fn fates(script: &[Step], world: &World, posted_allowed: bool) -> Vec<Fate> {
    let per = if posted_allowed { 3 } else { 2 };
    let combos = (0..world.device.len()).fold(1usize, |acc, _| acc * per);
    let mut out = Vec::new();
    for combo in 0..combos {
        let mut cache = world.cache;
        let mut media = world.media;
        let mut posted = world.posted.clone();
        let mut c = combo;
        for &(tag, request) in &world.device {
            match c % per {
                0 => {}
                1 => apply(script, &mut cache, &mut media, request),
                _ => {
                    apply(script, &mut cache, &mut media, request);
                    posted.push(tag);
                }
            }
            c /= per;
        }
        out.push((posted.clone(), cache, media));
        out.push((posted, [None; BLOCKS], media));
    }
    out
}

fn fail(run: &mut Run, law: Law, why: String) {
    if run.broken.is_none() {
        run.broken = Some((law, format!("{why}, after {}", run.path.join(" > "))));
    }
}

/// What the glue does after every event: take the client's answers, hold each
/// against the laws, and pump.
fn settle(script: &[Step], world: &mut World) -> Result<(), (Law, String)> {
    let answers: Vec<_> = world.client.take_answers().collect();
    let _ = world.client.take_released().count();
    for (ticket, outcome) in answers {
        let had = world.answers.entry(ticket).or_default();
        had.push(outcome);
        if had.len() > 1 {
            return Err((Law::Answers, format!("ticket {ticket} answered twice: {had:?}")));
        }
        if outcome == Outcome::Durable {
            durable(script, world, ticket).map_err(|why| (Law::Durable, why))?;
        }
    }
    pump(world);
    Ok(())
}

/// What the flush `ticket`, just answered durable, promised is on the medium.
fn durable(script: &[Step], world: &World, flush: Ticket) -> Result<(), String> {
    let before = &world.acked_before[&flush];
    for block in 0..BLOCKS as u64 {
        let on_block = |t: &Ticket| write_of(script, *t).is_some_and(|(b, _)| b == block);
        let Some(last) = before.iter().copied().filter(on_block).max() else { continue };
        let (_, want) = write_of(script, last).expect("a write");
        let allowed: Vec<u8> = core::iter::once(want)
            .chain((last + 1..script.len() as u64).filter(on_block).filter_map(|t| write_of(script, t).map(|(_, v)| v)))
            .collect();
        let on = world.media[block as usize];
        if !allowed.contains(&on) {
            return Err(format!(
                "flush {flush} was answered durable with block {block} holding {on}, not the \
                 {want} acknowledged before it (or a later one of {allowed:?})"
            ));
        }
    }
    Ok(())
}

/// Nothing more can happen: every ticket was answered, and every flush said
/// durable — or, past [`MAX_ATTEMPTS`] failures, said the device refused it.
fn end(run: &mut Run, world: &World) {
    let mut given_up = false;
    for ticket in 0..run.script.len() as Ticket {
        let answered = world.answers.get(&ticket).map(Vec::as_slice);
        let Some([outcome]) = answered else {
            fail(run, Law::Answers, format!("ticket {ticket} ended answered {answered:?}"));
            return;
        };
        if run.script[ticket as usize] != Step::Flush || *outcome == Outcome::Durable {
            continue;
        }
        if *outcome != Outcome::Device || !run.may_give_up {
            fail(run, Law::Durable, format!("flush {ticket} ended {outcome:?}"));
            return;
        }
        given_up = true;
    }
    if given_up {
        run.given_up += 1;
    }
}

/// Names a state's tags by first appearance.
#[derive(Default)]
struct Names(Vec<u32>);

impl Names {
    fn of(&mut self, tag: u32) -> u32 {
        let at = self.0.iter().position(|&t| t == tag).unwrap_or_else(|| {
            self.0.push(tag);
            self.0.len() - 1
        });
        at as u32
    }
}

/// A state's name in the search: everything it holds, with every tag renamed
/// by first appearance, the client's first in slot order, and the client's
/// table rendered by its filled slots.
fn key(world: &World) -> String {
    let mut names = Names::default();
    let wire: Vec<u32> = world.client.on_the_wire().map(|t| names.of(t)).collect();
    let mut request = |r: &Request| Request { tag: names.of(r.tag), ..*r };
    let sq: Vec<Request> = world.queues.sq.iter().map(&mut request).collect();
    let device: Vec<Request> = world.device.iter().map(|(_, r)| request(r)).collect();
    let cq: Vec<Completion> = world.queues.cq.iter().map(|c| Completion { tag: names.of(c.tag), ..*c }).collect();
    let server = world.server.as_ref().map(|s| {
        let inflight: Vec<(u32, Op)> = s.session.inflight().map(|(t, op)| (names.of(t), op)).collect();
        format!("{inflight:?} {:?}", s.holds)
    });
    let posted: Vec<u32> = world.posted.iter().map(|&t| names.of(t)).collect();
    format!(
        "{:?} {wire:?} {sq:?} {device:?} {cq:?} {server:?} {posted:?} {} {:?} {:?} {} {} {:?} {:?} {:?}",
        world.client,
        world.next,
        world.answers,
        world.acked_before,
        world.alive,
        world.losses,
        world.cache,
        world.media,
        world.left
    )
}

fn dfs(run: &mut Run, mut world: World) {
    world.queues.hold_empty();
    if run.broken.is_some() || !run.seen.insert(key(&world)) {
        return;
    }
    run.filled[0] |= world.queues.sq.len() == DEPTH as usize;
    run.filled[1] |= world.queues.cq.len() == DEPTH as usize;
    let next = next(run.script, &world);
    if next.is_empty() {
        run.ends += 1;
        end(run, &world);
    }
    for (step, after) in next {
        run.path.push(step);
        match after {
            Ok(world) => dfs(run, world),
            Err((law, why)) => fail(run, law, why),
        }
        run.path.pop();
    }
}

/// The world after an event, or the law it broke.
type After = Result<World, (Law, String)>;

/// Every event that can happen next, named, and what it leads to.
fn next(script: &[Step], world: &World) -> Vec<(String, After)> {
    let mut next = Vec::new();
    let mut after = |step: String, after: After| {
        next.push((step, after.and_then(|mut w| settle(script, &mut w).map(|()| w))));
    };

    // The caller asks for its next step, never a write to a block with a write
    // still unanswered: two writes in flight to one block are unordered.
    if world.next < script.len() {
        let ticket = world.next as Ticket;
        let busy = write_of(script, ticket).is_some_and(|(block, _)| {
            (0..ticket).any(|t| {
                write_of(script, t).is_some_and(|(b, _)| b == block) && !world.answers.contains_key(&t)
            })
        });
        if !busy {
            let mut w = world.clone();
            match script[world.next] {
                Step::Write { block, .. } => {
                    let run = ARENA.run(ticket as u32, 1).expect("a script's ticket is an arena block");
                    w.client.submit(ticket, Op::Write { run, lba: block });
                }
                Step::Flush => {
                    let acked = w
                        .answers
                        .iter()
                        .filter(|(t, a)| write_of(script, **t).is_some() && a.as_slice() == [Outcome::Done])
                        .map(|(t, _)| *t)
                        .collect();
                    w.acked_before.insert(ticket, acked);
                    w.client.submit(ticket, Op::Flush);
                }
            }
            w.next += 1;
            after(format!("ask {}", world.next), Ok(w));
        }
    }

    // The server takes the oldest request.
    if world.alive && !world.queues.sq.is_empty() {
        let mut w = world.clone();
        let words = w.queues.take().expect("just seen");
        let server = w.server.as_mut().expect("alive");
        let step = match server.session.take(words) {
            Taken::Issue(request) => {
                w.device.push((request.tag, request));
                format!("take {:?}#{}", request.op, request.tag)
            }
            Taken::Answer(c) => {
                w.queues.post(c);
                format!("refuse #{}", c.tag)
            }
        };
        after(step, Ok(w));
    }

    // The device completes any one command it holds.
    for i in 0..world.device.len() {
        let mut w = world.clone();
        let (tag, request) = w.device.remove(i);
        apply(script, &mut w.cache, &mut w.media, request);
        let losses = w.losses;
        if let Some(server) = w.server.as_mut() {
            if let Some(c) = server.session.complete(tag, true, &mut server.holds, losses) {
                w.queues.post(c);
            }
        }
        after(format!("done {:?}#{tag}", request.op), Ok(w));
    }

    // The device fails any one command it holds: not done, and answered so.
    if world.left.errors > 0 {
        for i in 0..world.device.len() {
            let mut w = world.clone();
            w.left.errors -= 1;
            let (tag, request) = w.device.remove(i);
            let losses = w.losses;
            if let Some(server) = w.server.as_mut() {
                if let Some(c) = server.session.complete(tag, false, &mut server.holds, losses) {
                    w.queues.post(c);
                }
            }
            after(format!("fail {:?}#{tag}", request.op), Ok(w));
        }
    }

    // The client reads the oldest completion.
    if world.client.up() && !world.queues.cq.is_empty() {
        let mut w = world.clone();
        let c = w.queues.read().expect("just seen");
        let read = w.client.complete(c).map(|()| w).map_err(|_| {
            (Law::Answers, format!("the client met a second completion for tag {}", c.tag))
        });
        after(format!("read #{} {:?}", c.tag, c.status), read);
    }

    // The server reads a completion the device posted before it was reset:
    // its request has already been answered.
    if world.alive && !world.posted.is_empty() {
        let mut w = world.clone();
        let tag = w.posted.remove(0);
        let losses = w.losses;
        let server = w.server.as_mut().expect("alive");
        if let Some(c) = server.session.complete(tag, true, &mut server.holds, losses) {
            w.queues.post(c);
        }
        after(format!("posted #{tag}"), Ok(w));
    }

    // The device is reset under everything in flight.
    if world.left.resets > 0 && world.alive {
        for (posted, cache, media) in fates(script, world, true) {
            let mut w = world.clone();
            w.left.resets -= 1;
            w.device.clear();
            w.posted = posted.clone();
            w.cache = cache;
            w.media = media;
            w.losses += 1;
            let server = w.server.as_mut().expect("alive");
            for c in server.session.abort_all() {
                w.queues.post(c);
            }
            after(format!("reset({posted:?} {cache:?} {media:?})"), Ok(w));
        }
    }

    // The server dies.
    if world.left.crashes > 0 && world.alive {
        for (_, cache, media) in fates(script, world, false) {
            let mut w = world.clone();
            w.left.crashes -= 1;
            w.device.clear();
            w.posted.clear();
            w.cache = cache;
            w.media = media;
            w.server = None;
            w.alive = false;
            after(format!("crash({cache:?} {media:?})"), Ok(w));
        }
    }

    // The client notices the session is over.
    if !world.alive && world.client.up() {
        let mut w = world.clone();
        w.client.session_ended();
        w.queues.reset();
        after("notice".into(), Ok(w));
    }

    // It reconnects to the server started in its place.
    if !world.alive && !world.client.up() {
        let mut w = world.clone();
        connect(&mut w);
        after("reconnect".into(), Ok(w));
    }

    next
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: [Step; 5] = [
        Step::Write { block: 0, value: 1 },
        Step::Write { block: 1, value: 2 },
        Step::Flush,
        Step::Write { block: 0, value: 3 },
        Step::Flush,
    ];
    const _: () = assert!(SCRIPT.len() > DEPTH as usize, "a ring never wraps");

    const fn at_most(resets: u8, crashes: u8, errors: u8) -> Failures {
        Failures { resets, crashes, errors }
    }

    /// The failures [`SCRIPT`] is held under: none, and each alone.
    const BOUNDS: [Failures; 4] = [at_most(0, 0, 0), at_most(1, 0, 0), at_most(0, 1, 0), at_most(0, 0, 1)];

    /// Two writes to one block with a flush after each: a write issued again
    /// after a loss has an earlier one to keep behind and a flush to answer
    /// for it.
    const ONE_BLOCK: [Step; 4] =
        [Step::Write { block: 0, value: 1 }, Step::Flush, Step::Write { block: 0, value: 3 }, Step::Flush];

    /// What [`ONE_BLOCK`] is held under: a reset beside a death. Not
    /// [`SCRIPT`], whose visited states under two failures outgrow the memory
    /// of the host job that runs this.
    const ONE_BLOCK_BOUND: Failures = at_most(1, 1, 0);

    /// One write and the flush after it, the shortest script a write can be
    /// given up in.
    const GIVE_UP: [Step; 2] = [Step::Write { block: 0, value: 1 }, Step::Flush];

    /// What it is held under: two deaths and resets, and a death followed by
    /// more device errors than [`MAX_ATTEMPTS`] — what makes the client give
    /// an acknowledged write up.
    const GIVE_UP_BOUNDS: [Failures; 3] = [
        at_most(2, 2, 0),
        at_most(0, 1, MAX_ATTEMPTS as u8 + 1),
        at_most(1, 1, MAX_ATTEMPTS as u8),
    ];

    fn every_bound() -> impl Iterator<Item = (&'static [Step], Failures)> {
        BOUNDS
            .into_iter()
            .map(|f| (&SCRIPT[..], f))
            .chain([(&ONE_BLOCK[..], ONE_BLOCK_BOUND)])
            .chain(GIVE_UP_BOUNDS.into_iter().map(|f| (&GIVE_UP[..], f)))
    }

    fn verdict(script: &[Step], failures: Failures) -> Explored {
        let explored = explore(script, failures);
        std::println!(
            "{failures:?}: {} end states, {} with a flush given up, {:?}",
            explored.ends,
            explored.given_up,
            explored.broken
        );
        explored
    }

    /// No request is answered twice or left unanswered, however a reset, a
    /// server's death and a device error land. A lost completion and a double
    /// completion are both this law.
    #[test]
    fn every_request_is_answered_exactly_once() {
        for (script, failures) in every_bound() {
            let explored = verdict(script, failures);
            assert!(explored.ends > 0, "the model reached no end");
            if let Some((Law::Answers, why)) = explored.broken {
                panic!("{failures:?}: {why}");
            }
        }
    }

    /// What a flush says is durable is on the medium, and a number of failures
    /// the client does not give up at never keeps a flush from saying it.
    #[test]
    fn what_a_flush_calls_durable_is_on_the_medium() {
        for (script, failures) in every_bound() {
            if let Some((Law::Durable, why)) = verdict(script, failures).broken {
                panic!("{failures:?}: {why}");
            }
        }
    }

    /// The model is not vacuous: with no failure at all, a run ends with both
    /// flushes durable and the medium holding the script's last values; each
    /// ring holds its depth, and wraps, because one session carries more than
    /// that.
    #[test]
    fn the_model_reaches_the_end_it_should() {
        let explored = verdict(&SCRIPT, at_most(0, 0, 0));
        assert_eq!(explored.broken, None);
        assert!(explored.ends >= 1);
        assert_eq!(explored.filled, [true, true], "a ring never held its depth");
    }

    /// Nor is the give-up path out of its reach: past [`MAX_ATTEMPTS`] failures
    /// some run ends with a flush answered the device's refusal, and the laws
    /// above were held on it.
    #[test]
    fn the_model_reaches_a_write_given_up() {
        let explored = verdict(&GIVE_UP, at_most(0, 1, MAX_ATTEMPTS as u8 + 1));
        assert_eq!(explored.broken, None);
        assert!(explored.given_up > 0, "no run gave a write up");
    }

    /// The world after each event in turn, each the first whose name starts with
    /// the word given.
    fn walk(mut world: World, events: &[&str]) -> World {
        for event in events {
            let (_, after) = next(&SCRIPT, &world)
                .into_iter()
                .find(|(step, _)| step.starts_with(event))
                .unwrap_or_else(|| panic!("no {event} after {}", key(&world)));
            world = after.expect("no law is broken on the way");
        }
        world
    }

    /// Two states a key that orders tags by value merges, though a reset parts
    /// them: F2 on the device and slot 1 free, reached with W1 asked after W0
    /// was answered, and with the two in flight together. Named alike, they act
    /// alike: with W3 taken and the device reset, they are still named alike.
    #[test]
    fn states_named_alike_act_alike() {
        let fresh = start(at_most(1, 0, 0));
        let answered_first =
            walk(fresh.clone(), &["ask", "take", "done", "read", "ask", "take", "done", "read", "ask", "take"]);
        let together = walk(fresh, &["ask", "ask", "take", "done", "read", "take", "done", "read", "ask", "take"]);
        assert_ne!(answered_first.device, together.device, "the flush on the device has one tag in both");
        assert_eq!(key(&answered_first), key(&together), "the search tells the two apart");
        let [answered_first, together] = [answered_first, together].map(|w| walk(w, &["ask", "take", "reset"]));
        assert_eq!(key(&answered_first), key(&together), "a reset answered the two in different orders");
    }
}
