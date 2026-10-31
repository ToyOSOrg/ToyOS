//! [`Served`] over a session these tests script: what it asks again across a
//! session's end, when it calls the disk gone, and that each answer reaches
//! the task that asked for it.

use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;

use toyos_blockdev::{block_on, Block, BlockDev, Error, Executor, SharedBlock, BLOCK};

use super::{Link, Served, MAX_RECONNECTS};
use crate::session::{Answer, Error as SessionError, Unsent, Waited};
use crate::{Outcome, Ticket};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Asked {
    Read { lba: u64, blocks: u32 },
    Write { lba: u64, data: Vec<u8> },
    Flush,
}

/// A session over a partition in memory. Everything asked is on the wire at
/// once and answered at the next wait, the latest first, unless the session
/// is scripted to end there.
struct Script {
    disk: Vec<u8>,
    next: Ticket,
    wire: Vec<(Ticket, Asked)>,
    /// Every request, in the order it was asked.
    log: Vec<Asked>,
    /// At each of these waits, counted from 0, the session ends with what is
    /// on the wire.
    ends: VecDeque<u32>,
    waits: u32,
    up: bool,
    /// Each reconnect's answer, in turn; `Ok` once they run out.
    reconnects: VecDeque<Result<(), SessionError>>,
    reconnected: u32,
    read_only: bool,
}

impl Script {
    fn new(blocks: usize) -> Self {
        Self {
            disk: vec![0; blocks * BLOCK],
            next: 0,
            wire: Vec::new(),
            log: Vec::new(),
            ends: VecDeque::new(),
            waits: 0,
            up: true,
            reconnects: VecDeque::new(),
            reconnected: 0,
            read_only: false,
        }
    }

    fn ask(&mut self, asked: Asked) -> Result<Ticket, Unsent> {
        if !self.up {
            return Err(Unsent::Ended);
        }
        let ticket = self.next;
        self.next += 1;
        self.log.push(asked.clone());
        self.wire.push((ticket, asked));
        Ok(ticket)
    }

    fn answer(&mut self, ticket: Ticket, asked: Asked) -> Answer {
        let (outcome, data) = match asked {
            Asked::Read { lba, blocks } => {
                let at = lba as usize * BLOCK;
                (Outcome::Done, Some(self.disk[at..at + blocks as usize * BLOCK].to_vec()))
            }
            Asked::Write { .. } if self.read_only => (Outcome::ReadOnly, None),
            Asked::Write { lba, data } => {
                let at = lba as usize * BLOCK;
                self.disk[at..at + data.len()].copy_from_slice(&data);
                (Outcome::Done, None)
            }
            Asked::Flush => (Outcome::Durable, None),
        };
        Answer { ticket, outcome, data }
    }
}

impl Link for Script {
    fn blocks(&self) -> u64 {
        (self.disk.len() / BLOCK) as u64
    }

    fn submit_read(&mut self, lba: u64, blocks: u32) -> Result<Ticket, Unsent> {
        self.ask(Asked::Read { lba, blocks })
    }

    fn submit_write(&mut self, lba: u64, blocks: &[SharedBlock]) -> Result<Ticket, Unsent> {
        self.ask(Asked::Write { lba, data: blocks.iter().flat_map(|b| b.iter().copied()).collect() })
    }

    fn submit_flush(&mut self) -> Result<Ticket, Unsent> {
        self.ask(Asked::Flush)
    }

    fn wait(&mut self, _block: bool) -> Waited {
        if !self.up {
            return Waited { answers: Vec::new(), ended: true };
        }
        let wait = self.waits;
        self.waits += 1;
        let wire = std::mem::take(&mut self.wire);
        if self.ends.front() == Some(&wait) {
            self.ends.pop_front();
            self.up = false;
            let answers = wire.into_iter().map(|(ticket, _)| Answer { ticket, outcome: Outcome::Refused, data: None });
            return Waited { answers: answers.collect(), ended: true };
        }
        Waited { answers: wire.into_iter().rev().map(|(t, a)| self.answer(t, a)).collect(), ended: false }
    }

    fn reconnect(&mut self) -> Result<(), SessionError> {
        assert!(!self.up, "a reconnect while the session is up");
        self.reconnected += 1;
        let answer = self.reconnects.pop_front().unwrap_or(Ok(()));
        self.up = answer.is_ok();
        answer
    }
}

fn now<T>(disk: &Served<Script>, call: impl Future<Output = T>) -> T {
    block_on(call, || disk.wait())
}

fn blocks(fill: u8, n: usize) -> Vec<Block> {
    vec![[fill; BLOCK]; n]
}

/// A read, a write and a flush each on the wire when the session ends: each
/// is asked again on the session the end reconnected, and answered there.
#[test]
fn a_request_on_the_wire_at_a_session_end_is_asked_again_after_the_reconnect() {
    let mut script = Script::new(8);
    script.disk[3 * BLOCK..4 * BLOCK].fill(0x33);
    script.ends.extend([0, 2, 4]);
    let disk = Served::new(script);

    let mut out = blocks(0, 1);
    now(&disk, disk.read(3, &mut out)).unwrap();
    assert_eq!(out, blocks(0x33, 1));
    now(&disk, disk.write(5, &blocks(0x55, 1))).unwrap();
    now(&disk, disk.flush()).unwrap();

    let script = disk.link.borrow();
    assert_eq!(script.reconnected, 3);
    let read = Asked::Read { lba: 3, blocks: 1 };
    let write = Asked::Write { lba: 5, data: vec![0x55; BLOCK] };
    assert_eq!(script.log, [read.clone(), read, write.clone(), write, Asked::Flush, Asked::Flush]);
    assert!(script.disk[5 * BLOCK..6 * BLOCK].iter().all(|&b| b == 0x55));
    assert!(!disk.pending());
}

/// A request refused at every session end is asked once more than
/// [`MAX_RECONNECTS`] allows again, and then the disk is gone to it.
#[test]
fn a_request_refused_at_more_ends_than_the_bound_is_answered_gone() {
    let mut script = Script::new(4);
    script.ends.extend(0..=MAX_RECONNECTS);
    let disk = Served::new(script);
    assert_eq!(now(&disk, disk.write(0, &blocks(1, 1))), Err(Error::Gone));
    assert_eq!(disk.link.borrow().log.len(), MAX_RECONNECTS as usize + 1);
    assert!(!disk.pending(), "the gone request is still counted as waiting");
}

/// A reconnect the service refuses makes the disk gone: the request on the
/// wire and every later one answer `Gone`, nothing is asked again, and no
/// reconnect is tried again.
#[test]
fn a_refused_reconnect_makes_the_disk_gone_for_every_request() {
    let mut script = Script::new(4);
    script.ends.push_back(0);
    script.reconnects.push_back(Err(SessionError::Unanswered));
    let disk = Served::new(script);

    assert_eq!(now(&disk, disk.write(1, &blocks(1, 1))), Err(Error::Gone));
    let mut out = blocks(0, 1);
    assert_eq!(now(&disk, disk.read(1, &mut out)), Err(Error::Gone));
    assert_eq!(now(&disk, disk.flush()), Err(Error::Gone));
    disk.progress(false);

    let script = disk.link.borrow();
    assert_eq!((script.log.len(), script.reconnected), (1, 1));
}

/// A transfer longer than one request carries is asked in requests of at
/// most `MAX_REQUEST_BLOCKS`; one past the partition's end is refused before
/// anything is asked; a write the grant does not make is `ReadOnly`.
#[test]
fn transfers_are_split_bounded_and_answered_in_the_device_word() {
    let mut script = Script::new(80);
    script.read_only = true;
    let disk = Served::new(script);
    let mut out = blocks(0, 70);
    now(&disk, disk.read(4, &mut out)).unwrap();
    let asked: Vec<_> = disk.link.borrow().log.clone();
    let reads = [(4, 32), (36, 32), (68, 6)].map(|(lba, blocks)| Asked::Read { lba, blocks });
    assert_eq!(asked, reads);

    assert_eq!(now(&disk, disk.read(75, &mut out[..6])), Err(Error::Range));
    assert_eq!(disk.link.borrow().log.len(), 3, "a range past the end was asked");
    assert_eq!(now(&disk, disk.write(0, &blocks(1, 1))), Err(Error::ReadOnly));
}

/// Two tasks with requests on the wire together, answered in the other
/// order: each gets its own blocks.
#[test]
fn answers_reach_the_tasks_that_asked_whatever_their_order() {
    let mut script = Script::new(4);
    script.disk[BLOCK..2 * BLOCK].fill(1);
    script.disk[2 * BLOCK..3 * BLOCK].fill(2);
    let disk = Served::new(script);
    let (first, second) = (Rc::new(Cell::new(0u8)), Rc::new(Cell::new(0u8)));
    let exec = Executor::new();
    for (lba, got) in [(1, Rc::clone(&first)), (2, Rc::clone(&second))] {
        let disk = &disk;
        exec.spawn(async move {
            let mut out = blocks(0, 1);
            disk.read(lba, &mut out).await.unwrap();
            got.set(out[0][0]);
        });
    }
    assert_eq!(exec.run_until_stalled(), 2, "both asked and waiting");
    assert_eq!(disk.link.borrow().wire.len(), 2, "both on the wire together");
    disk.progress(true);
    assert_eq!(exec.run_until_stalled(), 0);
    assert_eq!((first.get(), second.get()), (1, 2));
}
