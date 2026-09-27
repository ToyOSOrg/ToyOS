//! Every ordering of one client, a server, its death, the client's reconnect,
//! and a server that answers a tag it has answered before.
//!
//! The client puts requests on a real [`Producer`] and reads answers off a
//! real [`Consumer`], over words of its own, keeping them in a real
//! [`Inflight`]. [`explore`] runs, depth first and exhaustively, every
//! interleaving of: the client sending its next request, the server taking
//! one, answering any one it holds, **answering again any tag it has ever
//! taken**, **dying**, the client reading an answer, noticing the hang-up,
//! and reconnecting over the same words to a fresh server. A client that reads
//! a violation ends the session as it would a hang-up.
//!
//! The law: every request is answered exactly once — never twice, and by the
//! end of every run — and an answer read off the ring answers only a request
//! of the session it was sent in.

use std::cell::Cell;
use std::collections::HashSet;
use std::format;
use std::string::String;
use std::vec::Vec;

use crate::{Consumer, Cursors, Inflight, Place, Producer, Untrusted, Word};

const D: u32 = 2;
const REQUESTS: usize = 3;
const SQ: Place = Place { cursors: Cursors { head: 0, tail: 1, sleep: 2 }, entries: 3 };
const CQ: Place = Place { cursors: Cursors { head: 5, tail: 6, sleep: 7 }, entries: 8 };
const WORDS: usize = 10;

/// A word of a model that runs on one thread: every order is the program's.
#[derive(Clone, Debug)]
struct Shared(Cell<u32>);

impl Word for Shared {
    fn load(&self, _: core::sync::atomic::Ordering) -> u32 {
        self.0.get()
    }
    fn store(&self, value: u32, _: core::sync::atomic::Ordering) {
        self.0.set(value)
    }
    fn fence() {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    /// Read off the ring in session `session`.
    Read { session: u32 },
    /// Given by the session's end.
    Ended,
}

#[derive(Clone, Debug)]
struct Server {
    requests: Consumer<1, D>,
    replies: Producer<1, D>,
    held: Vec<u32>,
}

#[derive(Clone, Debug)]
struct World {
    page: Vec<Shared>,
    requests: Producer<1, D>,
    replies: Consumer<1, D>,
    inflight: Inflight<usize, { D as usize }>,
    next: usize,
    session: u32,
    /// The session each request was sent in.
    sent: [Option<u32>; REQUESTS],
    answers: [Vec<Answer>; REQUESTS],
    server: Option<Server>,
    /// The server died and the client has not noticed.
    hung_up: bool,
    /// Every tag any server took, which a replay names.
    taken: Vec<u32>,
    crashes: u8,
    replays: u8,
}

/// What a run found.
pub struct Explored {
    pub broken: Option<String>,
    pub states: usize,
    pub ends: usize,
    /// Answers the client refused as naming nothing in flight.
    pub refused: usize,
}

struct Run {
    seen: HashSet<String>,
    broken: Option<String>,
    ends: usize,
    refused: usize,
    path: Vec<String>,
}

fn plain(word: Untrusted<u32>) -> u32 {
    word.at_most(u32::MAX.into()).unwrap() as u32
}

impl World {
    fn new(crashes: u8, replays: u8) -> Self {
        let page: Vec<Shared> = (0..WORDS).map(|_| Shared(Cell::new(0))).collect();
        let mut world = Self {
            requests: Producer::new(&page, SQ).unwrap(),
            replies: Consumer::new(&page, CQ).unwrap(),
            page,
            inflight: Inflight::new(),
            next: 0,
            session: 0,
            sent: [None; REQUESTS],
            answers: Default::default(),
            server: None,
            hung_up: false,
            taken: Vec::new(),
            crashes,
            replays,
        };
        world.connect();
        world
    }

    /// A fresh server's ends, and the client's again, over the same words.
    fn connect(&mut self) {
        self.requests = Producer::new(&self.page, SQ).unwrap();
        self.replies = Consumer::new(&self.page, CQ).unwrap();
        self.server = Some(Server {
            requests: Consumer::new(&self.page, SQ).unwrap(),
            replies: Producer::new(&self.page, CQ).unwrap(),
            held: Vec::new(),
        });
    }

    /// Whether the client thinks it has a session.
    fn up(&self) -> bool {
        self.server.is_some() || self.hung_up
    }

    /// The session is over: every request in flight is answered by its end.
    fn end(&mut self) {
        self.server = None;
        self.hung_up = false;
        let answers = &mut self.answers;
        self.inflight.end(|_, request| answers[request].push(Answer::Ended));
    }

    /// The server posts an answer naming `tag`; `false` if the ring is full.
    fn post(&mut self, tag: u32) -> bool {
        let server = self.server.as_mut().unwrap();
        if !server.replies.push(&self.page, [tag]).unwrap() {
            return false;
        }
        let _ = server.replies.publish(&self.page).unwrap();
        true
    }
}

fn go(run: &mut Run, step: String, world: World) {
    run.path.push(step);
    dfs(run, world);
    run.path.pop();
}

fn fail(run: &mut Run, why: String) {
    if run.broken.is_none() {
        run.broken = Some(format!("{why}, after {}", run.path.join(" > ")));
    }
}

fn dfs(run: &mut Run, world: World) {
    if run.broken.is_some() || !run.seen.insert(format!("{world:?}")) {
        return;
    }
    let mut moved = false;

    // The client sends its next request, into a ring nobody may be reading.
    if world.up() && world.next < REQUESTS {
        let mut w = world.clone();
        if let Ok(tag) = w.inflight.insert(w.next) {
            assert!(w.requests.push(&w.page, [tag]).unwrap(), "more requests on the ring than in flight");
            let _ = w.requests.publish(&w.page).unwrap();
            w.sent[w.next] = Some(w.session);
            w.next += 1;
            moved = true;
            go(run, format!("send {}#{tag:x}", world.next), w);
        }
    }

    if let Some(server) = &world.server {
        // The server takes the oldest request.
        let mut w = world.clone();
        let s = w.server.as_mut().unwrap();
        if let Some([tag]) = s.requests.pop(&w.page).unwrap() {
            let tag = plain(tag);
            s.requests.release(&w.page).unwrap();
            s.held.push(tag);
            w.taken.push(tag);
            moved = true;
            go(run, format!("take #{tag:x}"), w);
        }

        // It answers any one it holds.
        for i in 0..server.held.len() {
            let mut w = world.clone();
            let tag = w.server.as_mut().unwrap().held.remove(i);
            if w.post(tag) {
                moved = true;
                go(run, format!("answer #{tag:x}"), w);
            }
        }

        // It answers again a tag it took, in this session or an earlier one.
        if world.replays > 0 {
            for &tag in &world.taken {
                let mut w = world.clone();
                w.replays -= 1;
                if w.post(tag) {
                    moved = true;
                    go(run, format!("replay #{tag:x}"), w);
                }
            }
        }

        // It dies; the client has not noticed.
        if world.crashes > 0 {
            let mut w = world.clone();
            w.crashes -= 1;
            w.server = None;
            w.hung_up = true;
            moved = true;
            go(run, "crash".into(), w);
        }
    }

    // The client reads the oldest answer, the server alive or not.
    if world.up() {
        let mut w = world.clone();
        if let Some([tag]) = w.replies.pop(&w.page).unwrap() {
            w.replies.release(&w.page).unwrap();
            let step = format!("read #{:x}", plain(tag));
            match w.inflight.answer(tag) {
                Ok(request) => {
                    if w.sent[request] != Some(w.session) {
                        let sent = w.sent[request];
                        return fail(run, format!("request {request} of session {sent:?} answered in {}", w.session));
                    }
                    w.answers[request].push(Answer::Read { session: w.session });
                    if w.answers[request].len() > 1 {
                        return fail(run, format!("request {request} answered {:?}", w.answers[request]));
                    }
                }
                Err(_) => {
                    run.refused += 1;
                    w.end();
                }
            }
            moved = true;
            go(run, step, w);
        }
    }

    // It notices the hang-up.
    if world.hung_up {
        let mut w = world.clone();
        w.end();
        moved = true;
        go(run, "notice".into(), w);
    }

    // It reconnects over the same words, as a new session.
    if !world.up() {
        let mut w = world.clone();
        w.session += 1;
        w.connect();
        moved = true;
        go(run, "reconnect".into(), w);
    }

    if !moved {
        run.ends += 1;
        for (request, answers) in world.answers.iter().enumerate() {
            if answers.len() != 1 {
                return fail(run, format!("request {request} ended answered {answers:?}"));
            }
        }
    }
}

/// Explore every run with at most `crashes` deaths and `replays` replays.
pub fn explore(crashes: u8, replays: u8) -> Explored {
    let mut run = Run { seen: HashSet::new(), broken: None, ends: 0, refused: 0, path: Vec::new() };
    dfs(&mut run, World::new(crashes, replays));
    Explored { broken: run.broken, states: run.seen.len(), ends: run.ends, refused: run.refused }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the session is held under: no failure, a death, a replay, and
    /// both twice.
    const BOUNDS: [(u8, u8); 4] = [(0, 0), (1, 0), (0, 1), (2, 2)];

    #[test]
    fn every_tag_is_answered_exactly_once() {
        for (crashes, replays) in BOUNDS {
            let explored = explore(crashes, replays);
            std::println!(
                "{crashes} crashes, {replays} replays: {} states, {} end states, {} answers refused",
                explored.states,
                explored.ends,
                explored.refused
            );
            assert!(explored.ends > 0, "the model reached no end");
            if let Some(why) = explored.broken {
                panic!("{crashes} crashes, {replays} replays: {why}");
            }
        }
    }

    /// The model is not vacuous: a replay reaches the client and is refused.
    #[test]
    fn the_model_reaches_a_replay_refused() {
        let explored = explore(0, 1);
        assert_eq!(explored.broken, None);
        assert!(explored.refused > 0, "no replay reached the client");
    }
}
