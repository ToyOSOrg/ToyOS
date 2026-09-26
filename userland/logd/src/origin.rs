//! One program's log, as this program reads it: the ring init made for it,
//! the name and pid init registered it under, and what reading it has cost.
//!
//! **Whose a record is comes from here and nowhere in the record.** The ring
//! is the program's; init named it; a record's own pid and thread are the
//! writer's word inside that identity, shown in the line only where the writer
//! is not the process init started — a child it spawned into its own slots.
//!
//! **A line is what a writer ended, not what a record held.** A record the
//! writer marked unended — its text was full, or the stream was flushed
//! mid-line — is held under its writer's pid and thread until the record that
//! ends the line, and the line is said whole. What is held is bounded: past
//! [`JOIN_BYTES`] a writer's line is said as far as it got, past
//! [`JOINS`] writers the oldest held line is, and a program's end or init's
//! flush says every held line.
//!
//! A ring's memory is the writer's to scribble, so everything read from it is
//! input: `toyos::log::ring` trusts only equalities and bounds, and this reads
//! at most [`ROUND_RECORDS`] records a round from each program so none can hold
//! the loop.

use toyos::log::region::{Body, Ring, LANES, RING_BYTES};
use toyos::log::ring::Reader;
use toyos::log::Severity;
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

/// The longest line held for its end: past it, what is held is said.
pub const JOIN_BYTES: usize = 64 * 1024;
/// Writers of one program whose lines are held at once.
pub const JOINS: usize = 16;

/// One line a writer ended, or that this program ended for it.
pub struct Said {
    pub at_ns: u64,
    pub pid: u32,
    pub tid: u32,
    pub severity: Severity,
    pub text: Vec<u8>,
    /// Index into the caller's origins.
    pub origin: usize,
}

/// What reading one program found beyond its lines, for the caller to say.
#[derive(Default)]
pub struct Counted {
    /// Records the ring had no room for when they were written.
    pub refused: u64,
    /// Records past the allowance, counted this round.
    pub suppressed: u64,
    /// Whether the allowance began suppressing this round.
    pub began_suppressing: bool,
}

/// A writer's line so far.
struct Held {
    pid: u32,
    tid: u32,
    said: Said,
}

/// Lines begun and not yet ended, oldest first.
#[derive(Default)]
pub struct Joins {
    held: Vec<Held>,
}

impl Joins {
    /// One record onto its writer's line: the line said once the record ends
    /// it, or once it is as long as a line is held.
    pub fn join(&mut self, index: usize, body: &Body, out: &mut Vec<Said>) {
        let at = self.held.iter().position(|h| (h.pid, h.tid) == (body.pid, body.tid));
        let mut held = match at {
            Some(at) => self.held.remove(at),
            // It ends a line this program does not hold: one said already at
            // its bound, or begun in records the ring had no room for.
            None if body.closes() => return,
            None => Held {
                pid: body.pid,
                tid: body.tid,
                said: Said {
                    at_ns: body.at_ns,
                    pid: body.pid,
                    tid: body.tid,
                    severity: body.severity().unwrap_or(Severity::Info),
                    text: Vec::new(),
                    origin: index,
                },
            },
        };
        held.said.text.extend_from_slice(body.text());
        // The line is as late as its last piece.
        held.said.at_ns = held.said.at_ns.max(body.at_ns);
        if !body.unended() || held.said.text.len() >= JOIN_BYTES {
            out.push(held.said);
            return;
        }
        if self.held.len() == JOINS {
            out.push(self.held.remove(0).said);
        }
        self.held.push(held);
    }

    /// Every line held for its end, said as far as it got.
    pub fn say_held(&mut self, out: &mut Vec<Said>) {
        out.extend(self.held.drain(..).map(|held| held.said));
    }
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
    joins: Joins,
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
            joins: Joins::default(),
        })
    }

    /// Up to [`ROUND_RECORDS`] records, the allowance applied at `now_ns`,
    /// and every line they end into `out`. Answers what it counted instead of
    /// reading.
    pub fn read(&mut self, index: usize, now_ns: u64, out: &mut Vec<Said>) -> Counted {
        let mut counted = Counted::default();
        let mut left = ROUND_RECORDS;
        let mut bodies: Vec<Body> = Vec::new();
        for (i, reader) in self.lanes.iter_mut().enumerate() {
            let lane = self.ring.lane(i);
            while left > 0 {
                let Some(body) = reader.next_lane(&lane) else { break };
                bodies.push(body);
                left -= 1;
            }
            counted.refused += reader.refused(&lane);
        }
        while left > 0 {
            let Some(body) = self.shared.next(&self.ring) else { break };
            bodies.push(body);
            left -= 1;
        }
        counted.refused += self.shared.refused(&self.ring);
        for body in bodies {
            let limit = if body.pid == self.pid { &self.own } else { &self.children };
            match limit.admit(now_ns) {
                Admit::Say { suppressed, last } => {
                    counted.suppressed += suppressed;
                    counted.began_suppressing |= last;
                    self.joins.join(index, &body, out);
                }
                Admit::Suppress => {}
            }
        }
        counted
    }

    /// Everything its writers left, once none is left to write: the records a
    /// lane or the shared ring still holds, every line held for its end, and a
    /// count of the positions taken and never published. No allowance: these
    /// are a program's last words.
    pub fn sweep(&mut self, index: usize, out: &mut Vec<Said>) -> u64 {
        let mut bodies: Vec<Body> = Vec::new();
        for (i, reader) in self.lanes.iter_mut().enumerate() {
            let lane = self.ring.lane(i);
            while let Some(body) = reader.next_lane(&lane) {
                bodies.push(body);
            }
        }
        let abandoned = self.shared.sweep(&self.ring, |body| bodies.push(body));
        for body in bodies {
            self.joins.join(index, &body, out);
        }
        self.joins.say_held(out);
        abandoned
    }

    /// Every line held for its end, said as far as it got.
    pub fn say_held(&mut self, out: &mut Vec<Said>) {
        self.joins.say_held(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toyos::log::region::FLAG_UNENDED;

    fn body(pid: u32, tid: u32, at_ns: u64, text: &[u8], unended: bool) -> Body {
        let mut body = Body::EMPTY;
        body.pid = pid;
        body.tid = tid;
        body.at_ns = at_ns;
        body.text[..text.len()].copy_from_slice(text);
        body.len = text.len() as u16;
        body.flags = if unended { FLAG_UNENDED } else { 0 };
        body
    }

    /// A line the writer ended with a record that says only that is said
    /// whole, and such a record with no line open says nothing.
    #[test]
    fn a_closing_record_ends_its_writers_line_and_nothing_else() {
        let (mut joins, mut out) = (Joins::default(), Vec::new());
        let mut closes = body(7, 0, 12, b"", false);
        closes.flags = toyos::log::region::FLAG_CLOSES;
        joins.join(0, &closes, &mut out);
        assert!(out.is_empty());
        joins.join(0, &body(7, 0, 10, b"prompt> ", true), &mut out);
        joins.join(0, &closes, &mut out);
        assert_eq!(texts(&out), [(0, &b"prompt> "[..])]);
    }

    fn texts(out: &[Said]) -> Vec<(u32, &[u8])> {
        out.iter().map(|s| (s.tid, s.text.as_slice())).collect()
    }

    /// Two writers' pieces interleaved in the ring are two lines, each whole,
    /// said when its own end arrives and stamped by its last piece.
    #[test]
    fn a_writers_pieces_are_its_line_whoever_wrote_between_them() {
        let (mut joins, mut out) = (Joins::default(), Vec::new());
        joins.join(0, &body(7, 1, 10, b"one ", true), &mut out);
        joins.join(0, &body(7, 2, 11, b"two ", true), &mut out);
        joins.join(0, &body(7, 1, 12, b"line", false), &mut out);
        assert_eq!(texts(&out), [(1, &b"one line"[..])]);
        assert_eq!(out[0].at_ns, 12);
        joins.join(0, &body(7, 2, 13, b"lines", false), &mut out);
        assert_eq!(texts(&out), [(1, &b"one line"[..]), (2, &b"two lines"[..])]);
        joins.say_held(&mut out);
        assert_eq!(out.len(), 2);
    }

    /// A line that never ends is said at its bound, and one begun and left is
    /// said when its writers are gone — nothing held is lost.
    #[test]
    fn what_is_held_is_bounded_and_said() {
        let (mut joins, mut out) = (Joins::default(), Vec::new());
        let piece = [b'x'; 984];
        let mut pieces = 0;
        while out.is_empty() {
            joins.join(0, &body(7, 0, pieces, &piece, true), &mut out);
            pieces += 1;
        }
        assert!(out[0].text.len() >= JOIN_BYTES && out[0].text.len() < JOIN_BYTES + piece.len());
        out.clear();
        for tid in 0..=JOINS as u32 {
            joins.join(0, &body(7, tid + 100, 0, b"begun", true), &mut out);
        }
        // One past the writers held says the oldest.
        assert_eq!(texts(&out), [(100, &b"begun"[..])]);
        out.clear();
        joins.say_held(&mut out);
        assert_eq!(out.len(), JOINS);
        joins.say_held(&mut out);
        assert_eq!(out.len(), JOINS);
    }
}
