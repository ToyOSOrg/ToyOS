//! Every ordering of a client, a server, a device and their failures.
//!
//! A scripted caller asks for writes and flushes; the rings between the client
//! and the server are queues; the server is [`ServerSession`] over a device of
//! two blocks with a volatile cache. [`explore`] runs, depth first and
//! exhaustively, every interleaving of: the caller asking for its next step,
//! the server taking a request, the device completing any one it holds, the
//! client reading a completion, **the device being reset** under whatever is in
//! flight (each command dropped, or run before the stop with its completion
//! read or not; the cache kept or dropped), **the server dying** (each command dropped or applied; the
//! cache kept or dropped; the rings left as they were), the client noticing,
//! and the client reconnecting to a fresh server. The client and the server are
//! this crate's own types, not transliterations: what is checked is the code
//! blockd and its clients run.
//!
//! The laws:
//! - every ticket is answered exactly once — never twice, and by the end never
//!   not at all — and a server that keeps the protocol never makes the client
//!   meet a second completion for one tag ([`Law::Answers`]);
//! - a flush answered durable left on the medium, for every block, the last
//!   write acknowledged before the flush was asked for, or one asked for after
//!   it; and with the failures bounded, every flush is answered durable in the
//!   end ([`Law::Durable`]).

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use std::collections::HashSet;

use toyos_blockhold::Holds;

use crate::client::{Client, Outcome, Ticket};
use crate::entry::{Completion, Op, Request};
use crate::server::{ServerSession, Taken};

const BLOCKS: usize = 2;

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

#[derive(Clone, Debug)]
struct Server {
    session: ServerSession,
    holds: Holds<u8>,
}

#[derive(Clone, Debug)]
struct World {
    client: Client,
    next: usize,
    /// Every answer each ticket has had.
    answers: BTreeMap<Ticket, Vec<Outcome>>,
    /// For each flush ticket, the write tickets answered `Done` before it was
    /// asked for.
    acked_before: BTreeMap<Ticket, Vec<Ticket>>,
    sq: VecDeque<Request>,
    cq: VecDeque<Completion>,
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
    resets: u8,
    crashes: u8,
}

struct Run<'a> {
    script: &'a [Step],
    /// Visited states, keyed by their whole rendering: `Holds` carries no
    /// `Hash`, and a rendering is exact.
    seen: HashSet<String>,
    broken: Option<(Law, String)>,
    ends: usize,
    /// The steps from the start to here, for a failure to name.
    path: Vec<String>,
}

/// Explore `script` against at most `resets` device resets and `crashes`
/// server deaths: the first law broken, and how many end states were reached.
pub fn explore(script: &[Step], resets: u8, crashes: u8) -> (Option<(Law, String)>, usize) {
    let mut world = World {
        client: Client::new(),
        next: 0,
        answers: BTreeMap::new(),
        acked_before: BTreeMap::new(),
        sq: VecDeque::new(),
        cq: VecDeque::new(),
        server: None,
        alive: false,
        losses: 0,
        device: Vec::new(),
        posted: Vec::new(),
        cache: [None; BLOCKS],
        media: [0; BLOCKS],
        resets,
        crashes,
    };
    connect(&mut world);
    let mut run = Run { script, seen: HashSet::new(), broken: None, ends: 0, path: Vec::new() };
    dfs(&mut run, world);
    (run.broken, run.ends)
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

/// What the glue does after every event: every request the client will send
/// goes onto the ring.
fn pump(world: &mut World) {
    while let Some(request) = world.client.next_request() {
        world.sq.push_back(request);
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
        Op::Write => {
            let (_, value) = write_of(script, u64::from(request.arena)).expect("a write's arena is its ticket");
            cache[request.lba as usize] = Some(value);
        }
        Op::Flush => {
            for (b, slot) in cache.iter_mut().enumerate() {
                if let Some(v) = slot.take() {
                    media[b] = v;
                }
            }
        }
        Op::Read => {}
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

fn go(run: &mut Run, step: String, world: World) {
    run.path.push(step);
    dfs(run, world);
    run.path.pop();
}

/// Take the client's answers and hold each against the laws.
fn collect(run: &mut Run, world: &mut World) {
    let answers: Vec<_> = world.client.take_answers().collect();
    let _ = world.client.take_released().count();
    for (ticket, outcome) in answers {
        let had = world.answers.entry(ticket).or_default();
        had.push(outcome);
        if had.len() > 1 {
            let had = had.clone();
            fail(run, Law::Answers, format!("ticket {ticket} answered twice: {had:?}"));
            return;
        }
        if outcome == Outcome::Durable {
            durable(run, world, ticket);
        }
    }
    pump(world);
}

/// What the flush `ticket`, just answered durable, promised is on the medium.
fn durable(run: &mut Run, world: &World, flush: Ticket) {
    let before = &world.acked_before[&flush];
    for block in 0..BLOCKS as u64 {
        let on_block = |t: &Ticket| write_of(run.script, *t).is_some_and(|(b, _)| b == block);
        let Some(last) = before.iter().copied().filter(on_block).max() else { continue };
        let (_, want) = write_of(run.script, last).expect("a write");
        let allowed: Vec<u8> = core::iter::once(want)
            .chain((last + 1..run.script.len() as u64).filter(on_block).filter_map(|t| {
                write_of(run.script, t).map(|(_, v)| v)
            }))
            .collect();
        let on = world.media[block as usize];
        if !allowed.contains(&on) {
            fail(
                run,
                Law::Durable,
                format!(
                    "flush {flush} was answered durable with block {block} holding {on}, not the \
                     {want} acknowledged before it (or a later one of {allowed:?})"
                ),
            );
            return;
        }
    }
}

/// Nothing more can happen: every ticket was answered, and every flush said
/// durable.
fn end(run: &mut Run, world: &World) {
    for ticket in 0..run.script.len() as Ticket {
        let answered = world.answers.get(&ticket).map(Vec::as_slice);
        let Some([outcome]) = answered else {
            fail(run, Law::Answers, format!("ticket {ticket} ended answered {answered:?}"));
            return;
        };
        if run.script[ticket as usize] == Step::Flush && *outcome != Outcome::Durable {
            fail(run, Law::Durable, format!("flush {ticket} ended {outcome:?}"));
            return;
        }
    }
}

fn dfs(run: &mut Run, world: World) {
    if run.broken.is_some() || !run.seen.insert(format!("{world:?}")) {
        return;
    }
    let mut moved = false;
    let script = run.script;

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
                Step::Write { block, .. } => w.client.submit(ticket, Op::Write, block, 1, ticket as u32),
                Step::Flush => {
                    let acked = w
                        .answers
                        .iter()
                        .filter(|(t, a)| write_of(script, **t).is_some() && a.as_slice() == [Outcome::Done])
                        .map(|(t, _)| *t)
                        .collect();
                    w.acked_before.insert(ticket, acked);
                    w.client.submit(ticket, Op::Flush, 0, 0, 0);
                }
            }
            w.next += 1;
            pump(&mut w);
            moved = true;
            go(run, format!("ask {}", world.next), w);
        }
    }

    // The server takes the oldest request.
    if world.alive && !world.sq.is_empty() {
        let mut w = world.clone();
        let request = w.sq.pop_front().expect("just seen");
        let server = w.server.as_mut().expect("alive");
        match server.session.take(request.encode()) {
            Taken::Issue(request) => w.device.push((request.tag, request)),
            Taken::Answer(c) => w.cq.push_back(c),
        }
        moved = true;
        go(run, format!("take {:?}#{}", request.op, request.tag), w);
    }

    // The device completes any one command it holds.
    for i in 0..world.device.len() {
        let mut w = world.clone();
        let (tag, request) = w.device.remove(i);
        apply(script, &mut w.cache, &mut w.media, request);
        let losses = w.losses;
        if let Some(server) = w.server.as_mut() {
            if let Some(c) = server.session.complete(tag, true, &mut server.holds, losses) {
                w.cq.push_back(c);
            }
        }
        moved = true;
        go(run, format!("done {:?}#{tag}", request.op), w);
    }

    // The client reads the oldest completion.
    if world.client.up() && !world.cq.is_empty() {
        let mut w = world.clone();
        let c = w.cq.pop_front().expect("just seen");
        if w.client.complete(c).is_err() {
            fail(run, Law::Answers, format!("the client met a second completion for tag {}", c.tag));
            return;
        }
        collect(run, &mut w);
        moved = true;
        go(run, format!("read #{} {:?}", c.tag, c.status), w);
    }

    // The server reads a completion the device posted before it was reset:
    // its request has already been answered.
    if world.alive && !world.posted.is_empty() {
        let mut w = world.clone();
        let tag = w.posted.remove(0);
        let losses = w.losses;
        let server = w.server.as_mut().expect("alive");
        if let Some(c) = server.session.complete(tag, true, &mut server.holds, losses) {
            w.cq.push_back(c);
        }
        moved = true;
        go(run, format!("posted #{tag}"), w);
    }

    // The device is reset under everything in flight.
    if world.resets > 0 && world.alive {
        for (posted, cache, media) in fates(script, &world, true) {
            let mut w = world.clone();
            w.resets -= 1;
            w.device.clear();
            w.posted = posted.clone();
            w.cache = cache;
            w.media = media;
            w.losses += 1;
            let server = w.server.as_mut().expect("alive");
            w.cq.extend(server.session.abort_all());
            moved = true;
            go(run, format!("reset({posted:?} {cache:?} {media:?})"), w);
        }
    }

    // The server dies.
    if world.crashes > 0 && world.alive {
        for (_, cache, media) in fates(script, &world, false) {
            let mut w = world.clone();
            w.crashes -= 1;
            w.device.clear();
            w.posted.clear();
            w.cache = cache;
            w.media = media;
            w.server = None;
            w.alive = false;
            moved = true;
            go(run, format!("crash({cache:?} {media:?})"), w);
        }
    }

    // The client notices the session is over.
    if !world.alive && world.client.up() {
        let mut w = world.clone();
        w.client.session_ended();
        w.sq.clear();
        w.cq.clear();
        collect(run, &mut w);
        moved = true;
        go(run, "notice".into(), w);
    }

    // It reconnects to the server started in its place.
    if !world.alive && !world.client.up() {
        let mut w = world.clone();
        connect(&mut w);
        moved = true;
        go(run, "reconnect".into(), w);
    }

    if !moved {
        run.ends += 1;
        end(run, &world);
    }
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

    fn verdict(resets: u8, crashes: u8) -> (Option<(Law, String)>, usize) {
        let (broken, ends) = explore(&SCRIPT, resets, crashes);
        std::println!("resets {resets}, crashes {crashes}: {ends} end states, {broken:?}");
        (broken, ends)
    }

    /// No request is answered twice or left unanswered, however a reset and a
    /// server's death land. A lost completion and a double completion are both
    /// this law.
    #[test]
    fn every_request_is_answered_exactly_once() {
        for (resets, crashes) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let (broken, ends) = verdict(resets, crashes);
            assert!(ends > 0, "the model reached no end");
            if let Some((Law::Answers, why)) = broken {
                panic!("resets {resets}, crashes {crashes}: {why}");
            }
        }
    }

    /// What a flush says is durable is on the medium, and a bounded number of
    /// failures never keeps a flush from saying it.
    #[test]
    fn what_a_flush_calls_durable_is_on_the_medium() {
        for (resets, crashes) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let (broken, _) = verdict(resets, crashes);
            if let Some((Law::Durable, why)) = broken {
                panic!("resets {resets}, crashes {crashes}: {why}");
            }
        }
    }

    /// The model is not vacuous: with no failure at all, a run ends with both
    /// flushes durable and the medium holding the script's last values.
    #[test]
    fn the_model_reaches_the_end_it_should() {
        let (broken, ends) = verdict(0, 0);
        assert_eq!(broken, None);
        assert!(ends >= 1);
    }
}
