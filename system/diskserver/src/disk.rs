//! A partition the block service serves, as a [`BlockDev`]: [`Served`].
//!
//! **Answers arrive when its owner reads them**: a request is put on the
//! session when it is asked, and its answer reaches the task awaiting it at
//! [`Served::progress`], which the owner calls when the session's connection
//! is readable or, as a [`toyos_blockdev::block_on`]'s `wait`, through
//! [`Served::wait`].
//!
//! **A block service that ends is survived, not hidden**: the end is found at
//! [`Served::progress`], which reconnects through the same name; the session
//! issues again every write no flush had covered (`toyos_blockring::client`),
//! and a request that was on the wire is asked again, because a block request
//! names its whole destination and means the same thing twice. A request
//! refused across more than [`MAX_RECONNECTS`] ends is answered
//! [`Error::Gone`], and so is every request once a reconnect fails: what was
//! waiting for the next session would otherwise go out under requests asked
//! since.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::future::poll_fn;
use std::task::{Poll, Waker};

use toyos_blockdev::{within, Block, BlockDev, Error, SharedBlock, BLOCK};
use toyos_blockring::layout::{ARENA, DEPTH};
use toyos_blockring::MAX_REQUEST_BLOCKS;

use crate::session::{Answer, Error as SessionError, Session, Unsent, Waited};
use crate::{Outcome, Ticket};

/// What [`Served`] drives: a session's requests and their answers, as
/// [`Session`] keeps them, whose contract this is.
pub trait Link {
    fn blocks(&self) -> u64;
    fn submit_read(&mut self, lba: u64, blocks: u32) -> Result<Ticket, Unsent>;
    fn submit_write(&mut self, lba: u64, blocks: &[SharedBlock]) -> Result<Ticket, Unsent>;
    fn submit_flush(&mut self) -> Result<Ticket, Unsent>;
    fn wait(&mut self, block: bool) -> Waited;
    fn reconnect(&mut self) -> Result<(), SessionError>;
}

impl Link for Session {
    fn blocks(&self) -> u64 {
        Session::blocks(self)
    }
    fn submit_read(&mut self, lba: u64, blocks: u32) -> Result<Ticket, Unsent> {
        Session::submit_read(self, lba, blocks)
    }
    fn submit_write(&mut self, lba: u64, blocks: &[SharedBlock]) -> Result<Ticket, Unsent> {
        Session::submit_write(self, lba, blocks)
    }
    fn submit_flush(&mut self) -> Result<Ticket, Unsent> {
        Session::submit_flush(self)
    }
    fn wait(&mut self, block: bool) -> Waited {
        Session::wait(self, block)
    }
    fn reconnect(&mut self) -> Result<(), SessionError> {
        Session::reconnect(self)
    }
}

/// How many times one request is asked again across block service restarts
/// before the disk is called gone: the supervisor gives up on a service that
/// ends faster than this.
pub const MAX_RECONNECTS: u32 = 4;

/// A partition the block service serves.
pub struct Served<L = Session> {
    link: RefCell<L>,
    blocks: u64,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    /// Asked, and not yet taken by the task that asked.
    asked: BTreeSet<Ticket>,
    answered: BTreeMap<Ticket, Answer>,
    waiting: BTreeMap<Ticket, Waker>,
    /// A reconnect failed: nothing is asked again.
    gone: bool,
}

/// Why a request was not answered: the disk is gone.
struct Gone;

impl<L: Link> Served<L> {
    pub fn new(link: L) -> Self {
        let blocks = link.blocks();
        Self { link: RefCell::new(link), blocks, state: RefCell::default() }
    }

    /// Whether a request is waiting for its answer.
    pub fn pending(&self) -> bool {
        !self.state.borrow().asked.is_empty()
    }

    /// Every answer the session has, each to the task awaiting it; with
    /// `block`, once there is one or the session has ended. A session that
    /// ended is reconnected here.
    pub fn progress(&self, block: bool) {
        let Waited { answers, ended } = self.link.borrow_mut().wait(block);
        let mut woken = Vec::new();
        {
            let mut state = self.state.borrow_mut();
            for answer in answers {
                woken.extend(state.waiting.remove(&answer.ticket));
                state.answered.insert(answer.ticket, answer);
            }
        }
        if ended && !self.state.borrow().gone {
            println!("the block service ended; reconnecting");
            if let Err(why) = self.link.borrow_mut().reconnect() {
                println!("the block service would not take the session back ({why:?}); the disk is gone");
                let mut state = self.state.borrow_mut();
                state.gone = true;
                woken.extend(std::mem::take(&mut state.waiting).into_values());
            }
        }
        woken.into_iter().for_each(Waker::wake);
    }

    /// [`Served::progress`], blocking: the `wait` of a
    /// [`toyos_blockdev::block_on`] over this disk alone.
    pub fn wait(&self) {
        assert!(self.pending(), "diskserver: a wait on a disk with nothing asked of it would never end");
        self.progress(true);
    }

    /// How many requests of [`MAX_REQUEST_BLOCKS`] the session carries at
    /// once: the most a caller gains by having in flight.
    pub fn depth(&self) -> usize {
        (DEPTH as usize).min(ARENA.slots() as usize / MAX_REQUEST_BLOCKS as usize)
    }

    /// One request, asked again after each session end it was on the wire
    /// for.
    async fn ask(&self, mut submit: impl FnMut(&mut L) -> Result<Ticket, Unsent>) -> Result<Answer, Gone> {
        let mut refusals = 0;
        loop {
            if self.state.borrow().gone {
                return Err(Gone);
            }
            // `progress` reconnects as it finds an end, so a session still
            // ended is one that would not reconnect.
            let Ok(ticket) = submit(&mut self.link.borrow_mut()) else { return Err(Gone) };
            self.state.borrow_mut().asked.insert(ticket);
            let answer = self.answer(ticket).await?;
            if answer.outcome != Outcome::Refused {
                return Ok(answer);
            }
            refusals += 1;
            if refusals > MAX_RECONNECTS {
                println!("a block request was refused across {refusals} session ends; the disk is gone");
                return Err(Gone);
            }
        }
    }

    async fn answer(&self, ticket: Ticket) -> Result<Answer, Gone> {
        poll_fn(|cx| {
            let mut state = self.state.borrow_mut();
            if let Some(answer) = state.answered.remove(&ticket) {
                state.asked.remove(&ticket);
                return Poll::Ready(Ok(answer));
            }
            if state.gone {
                state.asked.remove(&ticket);
                return Poll::Ready(Err(Gone));
            }
            state.waiting.insert(ticket, cx.waker().clone());
            Poll::Pending
        })
        .await
    }

    /// `blocks` written at `lba`, shared: the session keeps them until a
    /// flush covers the write, rather than a copy of them.
    pub async fn write_shared(&self, lba: u64, blocks: &[SharedBlock]) -> Result<(), Error> {
        within(lba, blocks.len(), self.blocks)?;
        let per = MAX_REQUEST_BLOCKS as usize;
        for (i, chunk) in blocks.chunks(per).enumerate() {
            let at = lba + (i * per) as u64;
            match self.ask(|link| link.submit_write(at, chunk)).await.map_err(|Gone| Error::Gone)?.outcome {
                Outcome::Done => {}
                Outcome::ReadOnly => return Err(Error::ReadOnly),
                _ => return Err(Error::Device),
            }
        }
        Ok(())
    }
}

impl<L: Link> BlockDev for Served<L> {
    fn blocks(&self) -> u64 {
        self.blocks
    }

    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        within(lba, out.len(), self.blocks)?;
        let per = MAX_REQUEST_BLOCKS as usize;
        for (i, chunk) in out.chunks_mut(per).enumerate() {
            let (at, n) = (lba + (i * per) as u64, chunk.len() as u32);
            let answer = self.ask(|link| link.submit_read(at, n)).await.map_err(|Gone| Error::Gone)?;
            let chunk = chunk.as_flattened_mut();
            match (answer.outcome, answer.data) {
                (Outcome::Done, Some(data)) if data.len() == chunk.len() => chunk.copy_from_slice(&data),
                _ => return Err(Error::Device),
            }
        }
        Ok(())
    }

    /// A copy of `data`, which the session keeps until a flush covers it.
    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
        let shared: Vec<SharedBlock> = data.iter().map(|block| std::rc::Rc::new(Box::new(*block))).collect();
        self.write_shared(lba, &shared).await
    }

    async fn flush(&self) -> Result<(), Error> {
        match self.ask(Link::submit_flush).await {
            Ok(Answer { outcome: Outcome::Durable, .. }) => Ok(()),
            Ok(_) => Err(Error::Device),
            Err(Gone) => Err(Error::Gone),
        }
    }
}

const _: () = assert!(BLOCK == toyos_blockring::BLOCK_BYTES);

#[cfg(test)]
mod tests;
