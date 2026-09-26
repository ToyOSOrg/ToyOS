//! One program's log, as this program reads it: the ring init made for it,
//! the name and pid init registered it under, and what reading it has cost.
//!
//! **Whose a record is comes from here and nowhere in the record.** The ring
//! is the program's; init named it; a record's own pid and thread are the
//! writer's word inside that identity, shown in the line only where the writer
//! is not the process init started — a child it spawned into its own slots.
//!
//! A ring's memory is the writer's to scribble, so everything read from it is
//! input: `toyos::log::ring` trusts only equalities and bounds, and this reads
//! at most [`ROUND_RECORDS`] records a round from each program so none can hold
//! the loop.

use toyos::log::region::{Body, Ring, LANES, RING_BYTES};
use toyos::log::ring::Reader;
use toyos::shm::SharedMemory;
use toyos::Pipe;
use toyos_elide::limit::{Admit, Limit};
use toyos_logstream::Tag;

/// Records read from one program in one round, lanes and shared ring
/// together: a bound on the round, not on the program — the rest is read the
/// next round, and a program that outruns it fills its own ring.
pub const ROUND_RECORDS: usize = 512;

/// Records one program may put in the log a second before the rest of the
/// second's are counted instead: the per-program allowance. Generous — a
/// program printing its whole output at boot fits — and a flood is what it
/// stops. The process init started and the children it spawned into its own
/// slots are allowed apart, so a child's flood does not silence its parent
/// saying the child is done.
pub const ALLOWANCE: u64 = 4096;
pub const ALLOWANCE_WINDOW_NS: u64 = 1_000_000_000;

/// A record read, and whose it is.
pub struct Read {
    pub body: Body,
    /// Index into the caller's origins.
    pub origin: usize,
}

/// What reading one program found beyond its records, for the caller to say.
#[derive(Default)]
pub struct Counted {
    /// Records the ring had no room for when they were written.
    pub refused: u64,
    /// Records past the allowance, counted this round.
    pub suppressed: u64,
    /// Whether the allowance began suppressing this round.
    pub began_suppressing: bool,
}

pub struct Origin {
    pub tag: String,
    pub pid: u32,
    /// The mapping; the view below is over it.
    _region: SharedMemory,
    ring: Ring,
    pub alive: Pipe,
    shared: Reader,
    lanes: [Reader; LANES],
    /// The allowance of the process init started, and of every other writer.
    own: Limit,
    children: Limit,
}

impl Origin {
    /// Map a ring init sent, under the name and pid it sent with it.
    pub fn open(tag: Tag<'_>, pid: u32, ring: toyos::RawHandle, alive: Pipe) -> Result<Self, String> {
        let region = SharedMemory::adopt(ring, RING_BYTES)
            .map_err(|e| format!("{}'s ring will not map: {e:?}", tag.as_str()))?;
        let base = core::ptr::NonNull::new(region.as_ptr()).ok_or("a mapped ring at null")?;
        // SAFETY: `region` maps `RING_BYTES` and lives as long as this origin,
        // which is as long as the view is used.
        let ring = unsafe { Ring::at(base) };
        if !ring.is_laid_out() {
            return Err(format!("{}'s region is not a log ring", tag.as_str()));
        }
        Ok(Self {
            tag: tag.as_str().to_string(),
            pid,
            _region: region,
            ring,
            alive,
            shared: Reader::new(),
            lanes: [Reader::new(); LANES],
            own: Limit::new(ALLOWANCE, ALLOWANCE_WINDOW_NS),
            children: Limit::new(ALLOWANCE, ALLOWANCE_WINDOW_NS),
        })
    }

    /// Up to [`ROUND_RECORDS`] records into `out`, the allowance applied at
    /// `now_ns`. Answers what it counted instead of reading.
    pub fn read(&mut self, index: usize, now_ns: u64, out: &mut Vec<Read>) -> Counted {
        let mut counted = Counted::default();
        let mut left = ROUND_RECORDS;
        let (pid, own, children) = (self.pid, &self.own, &self.children);
        let mut take = |body: Body, counted: &mut Counted| {
            let limit = if body.pid == pid { own } else { children };
            match limit.admit(now_ns) {
                Admit::Say { suppressed, last } => {
                    counted.suppressed += suppressed;
                    counted.began_suppressing |= last;
                    out.push(Read { body, origin: index });
                }
                Admit::Suppress => {}
            }
        };
        for (i, reader) in self.lanes.iter_mut().enumerate() {
            let lane = self.ring.lane(i);
            while left > 0 {
                let Some(body) = reader.next_lane(&lane) else { break };
                take(body, &mut counted);
                left -= 1;
            }
            counted.refused += reader.refused(&lane);
        }
        while left > 0 {
            let Some(body) = self.shared.next(&self.ring) else { break };
            take(body, &mut counted);
            left -= 1;
        }
        counted.refused += self.shared.refused(&self.ring);
        counted
    }

    /// Everything its writers left, once none is left to write: the records a
    /// lane or the shared ring still holds, and a count of the positions taken
    /// and never published. No allowance: these are a program's last words.
    pub fn sweep(&mut self, index: usize, out: &mut Vec<Read>) -> u64 {
        for (i, reader) in self.lanes.iter_mut().enumerate() {
            let lane = self.ring.lane(i);
            while let Some(body) = reader.next_lane(&lane) {
                out.push(Read { body, origin: index });
            }
        }
        self.shared.sweep(&self.ring, |body| out.push(Read { body, origin: index }))
    }
}
