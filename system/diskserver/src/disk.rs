//! A partition as a program that reads and writes its blocks holds it: one
//! the block service serves ([`Served`]).
//!
//! **A block service that ends is survived, not hidden**: a [`Served`] disk
//! whose session ended reconnects through the same name and issues again every
//! write no flush had covered (`toyos_blockring::client`), then asks what it
//! was asking. What was on the wire when the session ended is asked again
//! because a block write names its whole destination and means the same thing
//! twice.

use crate::{Error as SessionError, Outcome, Session};
use toyos_blockring::MAX_REQUEST_BLOCKS;

pub use toyos_disk::{span, Disk, DiskError, BLOCK};

/// A partition the block service serves.
pub struct Served {
    session: Session,
    blocks: u64,
}

/// How many times one request is asked again across block service restarts
/// before the disk is called gone: the supervisor gives up on a service that ends
/// faster than this.
const MAX_RECONNECTS: u32 = 4;

impl Served {
    pub fn new(session: Session) -> Self {
        let blocks = session.blocks();
        Self { session, blocks }
    }

    /// Ask, and across a session's end, reconnect and ask again.
    fn asked<T>(&mut self, mut ask: impl FnMut(&mut Session) -> Result<T, SessionError>) -> Result<T, DiskError> {
        let mut reconnects = 0;
        loop {
            match ask(&mut self.session) {
                Ok(answer) => return Ok(answer),
                Err(SessionError::Ended) if reconnects < MAX_RECONNECTS => {
                    reconnects += 1;
                    println!("the block service ended; reconnecting ({reconnects} of {MAX_RECONNECTS})");
                    if let Err(why) = self.session.reconnect() {
                        println!("the block service would not take the session back: {why:?}");
                        return Err(DiskError::Gone);
                    }
                }
                Err(_) => return Err(DiskError::Gone),
            }
        }
    }
}

impl Disk for Served {
    fn blocks(&self) -> u64 {
        self.blocks
    }

    fn read(&mut self, first: u64, out: &mut [u8]) -> Result<(), DiskError> {
        span(first, out.len(), self.blocks)?;
        let per = MAX_REQUEST_BLOCKS as usize;
        for (i, chunk) in out.chunks_mut(BLOCK * per).enumerate() {
            let n = (chunk.len() / BLOCK) as u32;
            let at = first + (i * per) as u64;
            let (outcome, data) = self.asked(|s| s.read(at, n))?;
            match (outcome, data) {
                (Outcome::Done, Some(data)) if data.len() == chunk.len() => chunk.copy_from_slice(&data),
                (Outcome::Refused, _) => return Err(DiskError::Gone),
                _ => return Err(DiskError::Device),
            }
        }
        Ok(())
    }

    fn write(&mut self, first: u64, data: &[u8]) -> Result<(), DiskError> {
        span(first, data.len(), self.blocks)?;
        let per = MAX_REQUEST_BLOCKS as usize;
        for (i, chunk) in data.chunks(BLOCK * per).enumerate() {
            let at = first + (i * per) as u64;
            let mut outcome = self.asked(|s| s.write(at, chunk))?;
            // On the wire when the session ended: the write names its whole
            // destination, so it is written again rather than guessed at.
            if outcome == Outcome::Refused {
                outcome = self.asked(|s| s.write(at, chunk))?;
            }
            match outcome {
                Outcome::Done => {}
                Outcome::ReadOnly => return Err(DiskError::ReadOnly),
                _ => return Err(DiskError::Device),
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DiskError> {
        for _ in 0..=MAX_RECONNECTS {
            match self.asked(Session::flush)? {
                Outcome::Durable => return Ok(()),
                // The session ended under it: the client issues its covered
                // writes again first on the new one, then the flush is asked.
                Outcome::Refused => continue,
                _ => return Err(DiskError::Device),
            }
        }
        Err(DiskError::Gone)
    }
}
