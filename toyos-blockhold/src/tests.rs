use super::*;

/// `logd`'s `/log` on the boot stick: a writer whose writes the departed
/// device lost and who writes nothing after it, while another writer's flush
/// comes first.
#[test]
fn a_writer_silent_since_the_loss_is_told_when_another_flushes_first() {
    let mut holds = Holds::new();
    holds.hold(0, 8, 'L').unwrap();
    holds.hold(8, 16, 'S').unwrap();
    holds.wrote(Writer::Span(0), 0);
    // The device leaves owing that write, and comes back counting it.
    holds.wrote(Writer::Span(8), 1);
    assert_eq!(holds.flushed(Writer::Span(8), 1), Ok(()), "the other writer lost nothing");
    assert_eq!(holds.flushed(Writer::Span(0), 1), Err(Lost { holder: Some('L') }));
    assert_eq!(holds.flushed(Writer::Span(0), 1), Ok(()), "a loss is told once");
    assert!(!holds.untold(1));
}

/// A flush settles every writer's account: writes it made durable are not
/// failed by a departure after it.
#[test]
fn writes_an_earlier_flush_made_durable_are_not_lost() {
    let mut holds = Holds::new();
    holds.hold(0, 8, 'E').unwrap();
    holds.hold(8, 16, 'S').unwrap();
    holds.wrote(Writer::Span(0), 0);
    holds.wrote(Writer::Span(8), 0);
    assert_eq!(holds.flushed(Writer::Span(8), 0), Ok(()));
    // A departure after it, owing a write of the unspanned writer's.
    holds.wrote(Writer::Unspanned, 0);
    assert_eq!(holds.flushed(Writer::Span(0), 1), Ok(()), "its writes were durable");
    assert_eq!(holds.flushed(Writer::Unspanned, 1), Err(Lost { holder: None }));
}

/// A writer that writes again after the loss finds it at that write.
#[test]
fn a_write_after_the_loss_finds_it() {
    let mut holds = Holds::new();
    holds.hold(0, 8, 'D').unwrap();
    holds.wrote(Writer::Span(0), 0);
    holds.wrote(Writer::Span(0), 1);
    assert_eq!(holds.flushed(Writer::Span(0), 1), Err(Lost { holder: Some('D') }));
    assert_eq!(holds.flushed(Writer::Span(0), 1), Ok(()));
}

/// Write, the device leaves, close, claim again, fsync: the updater's
/// sequence, and the flush fails.
#[test]
fn a_loss_outlives_the_hold_that_wrote_it() {
    let mut holds = Holds::new();
    holds.hold(0, 8, 'D').unwrap();
    holds.wrote(Writer::Span(0), 0);
    holds.release(0);
    assert!(holds.untold(1), "released owing it, nobody has been told");
    holds.hold(0, 8, 'D').unwrap();
    assert_eq!(holds.flushed(Writer::Span(0), 1), Err(Lost { holder: Some('D') }));
    assert!(!holds.untold(1));
}

/// A write released unflushed is still owed by its blocks: a departure after
/// the release is told to whoever holds them next.
#[test]
fn a_write_released_unflushed_is_lost_to_the_next_holder() {
    let mut holds = Holds::new();
    holds.hold(0, 8, 'D').unwrap();
    holds.wrote(Writer::Span(0), 0);
    holds.release(0);
    assert!(!holds.untold(0), "not lost yet");
    holds.hold(8, 16, 'S').unwrap();
    holds.wrote(Writer::Span(8), 1);
    assert_eq!(holds.flushed(Writer::Span(8), 1), Ok(()));
    assert!(holds.untold(1));
    holds.hold(2, 4, 'P').unwrap();
    assert_eq!(holds.flushed(Writer::Span(2), 1), Err(Lost { holder: Some('P') }));
    // The blocks the new hold did not cover still owe it.
    assert!(holds.untold(1));
    holds.hold(4, 6, 'Q').unwrap();
    assert_eq!(holds.flushed(Writer::Span(4), 1), Err(Lost { holder: Some('Q') }));
}

/// A span released owing nothing leaves nothing behind.
#[test]
fn a_clean_release_leaves_nothing() {
    let mut holds = Holds::new();
    holds.hold(0, 8, 'D').unwrap();
    holds.wrote(Writer::Span(0), 0);
    assert_eq!(holds.flushed(Writer::Span(0), 0), Ok(()));
    holds.release(0);
    assert!(holds.spans.is_empty());
    holds.hold(0, 8, 'D').unwrap();
    holds.release(0);
    holds.wrote(Writer::Unspanned, 0);
    holds.hold(0, 8, 'E').unwrap();
    holds.release(0);
    assert_eq!(holds.flushed(Writer::Unspanned, 0), Ok(()));
    assert!(holds.spans.is_empty(), "a flush drops what it settled clean");
}

/// A held block refuses every overlapping hold with its holder's name;
/// touching spans do not overlap.
#[test]
fn a_block_has_one_holder() {
    let mut holds = Holds::new();
    holds.hold(4, 8, 'A').unwrap();
    assert_eq!(holds.hold(7, 9, 'B'), Err('A'));
    assert_eq!(holds.hold(0, 5, 'B'), Err('A'));
    assert_eq!(holds.hold(0, 16, 'B'), Err('A'));
    assert_eq!(holds.hold(0, 4, 'B'), Ok(()));
    assert_eq!(holds.hold(8, 9, 'C'), Ok(()));
    holds.release(4);
    assert_eq!(holds.hold(5, 6, 'D'), Ok(()));
}

// The model: every sequence of holds, releases, writes, flushes and
// departures up to `DEPTH`, over three spans of a two-block device — `A` and
// `B` one block each, `C` both — and the writer that holds none, judged by an
// oracle that knows which block's writes the disk lost. A released loss
// passes to the next hold of any of its blocks, whole: the oracle spreads it
// over that hold's blocks, which is the contract's over-report and nothing
// more.

const SPANS: [(char, u64, u64); 3] = [('A', 0, 1), ('B', 1, 2), ('C', 0, 2)];
/// Blocks 0 and 1, and the unspanned writer as a third.
const REGIONS: usize = 3;
const DEPTH: usize = 12;

#[derive(Clone, Copy, Debug)]
enum Step {
    Hold(usize),
    Release(usize),
    Write(Option<usize>),
    Flush(Option<usize>),
    Depart,
}

#[derive(Clone)]
struct Model {
    holds: Holds<char>,
    held: [bool; 3],
    losses: u64,
    /// Written since the device's last flush that succeeded.
    written: [bool; REGIONS],
    /// Lost, and not yet reported.
    lost: [bool; REGIONS],
}

fn regions(writer: Option<usize>) -> core::ops::Range<usize> {
    match writer {
        None => 2..3,
        Some(s) => SPANS[s].1 as usize..SPANS[s].2 as usize,
    }
}

fn writer(s: Option<usize>) -> Writer {
    s.map_or(Writer::Unspanned, |s| Writer::Span(SPANS[s].1))
}

impl Model {
    fn live(&self, s: Option<usize>) -> bool {
        s.map_or(true, |s| self.held[s])
    }

    fn step(&mut self, step: Step, trace: &[Step]) -> bool {
        match step {
            Step::Hold(s) => {
                let (name, first, end) = SPANS[s];
                let overlaps = |o: &usize| self.held[*o] && SPANS[*o].1 < end && first < SPANS[*o].2;
                let blockers: Vec<char> = (0..3).filter(overlaps).map(|o| SPANS[o].0).collect();
                match self.holds.hold(first, end, name) {
                    Err(by) => assert!(blockers.contains(&by), "refused by {by}: {trace:?}"),
                    Ok(()) => {
                        assert!(blockers.is_empty(), "held over {blockers:?}: {trace:?}");
                        self.held[s] = true;
                        for flags in [&mut self.written, &mut self.lost] {
                            let any = regions(Some(s)).any(|r| flags[r]);
                            regions(Some(s)).for_each(|r| flags[r] |= any);
                        }
                    }
                }
            }
            Step::Release(s) => {
                if !self.held[s] {
                    return false;
                }
                self.holds.release(SPANS[s].1);
                self.held[s] = false;
            }
            Step::Write(s) => {
                if !self.live(s) {
                    return false;
                }
                self.holds.wrote(writer(s), self.losses);
                regions(s).for_each(|r| self.written[r] = true);
            }
            Step::Flush(s) => {
                if !self.live(s) {
                    return false;
                }
                let want = match regions(s).any(|r| self.lost[r]) {
                    true => Err(Lost { holder: s.map(|s| SPANS[s].0) }),
                    false => Ok(()),
                };
                assert_eq!(self.holds.flushed(writer(s), self.losses), want, "{trace:?}");
                self.written = [false; REGIONS];
                regions(s).for_each(|r| self.lost[r] = false);
            }
            Step::Depart => {
                if self.written.iter().any(|&w| w) {
                    self.losses += 1;
                    for r in 0..REGIONS {
                        self.lost[r] |= self.written[r];
                    }
                    self.written = [false; REGIONS];
                }
            }
        }
        assert_eq!(self.holds.untold(self.losses), self.lost.iter().any(|&l| l), "{trace:?}");
        true
    }
}

/// Everything a sequence's future depends on, spans in block order, and the
/// depth it was met at.
type Key = (Vec<(u64, u64, Option<char>, Option<u64>, bool)>, Account, [bool; 3], [bool; REGIONS], [bool; REGIONS], u64, usize);

impl Model {
    fn key(&self, depth: usize) -> Key {
        let mut spans: Vec<_> = self
            .holds
            .spans
            .iter()
            .map(|s| (s.first, s.end, s.holder, s.account.unflushed, s.account.lost))
            .collect();
        spans.sort_unstable();
        (spans, self.holds.unspanned, self.held, self.written, self.lost, self.losses, depth)
    }
}

/// Every sequence of `DEPTH` steps from `model`, a state met again at one
/// depth explored once.
fn explore(model: &Model, trace: &mut Vec<Step>, seen: &mut std::collections::HashSet<Key>) {
    if trace.len() == DEPTH || !seen.insert(model.key(trace.len())) {
        return;
    }
    let every = (0..3)
        .flat_map(|s| [Step::Hold(s), Step::Release(s)])
        .chain([None, Some(0), Some(1), Some(2)].into_iter().flat_map(|s| [Step::Write(s), Step::Flush(s)]))
        .chain([Step::Depart]);
    for step in every {
        let mut next = model.clone();
        trace.push(step);
        if next.step(step, trace) {
            explore(&next, trace, seen);
        }
        trace.pop();
    }
}

#[test]
fn every_interleaving_tells_each_loss_to_the_blocks_it_touched() {
    let model = Model {
        holds: Holds::new(),
        held: [false; 3],
        losses: 0,
        written: [false; REGIONS],
        lost: [false; REGIONS],
    };
    let mut seen = std::collections::HashSet::new();
    explore(&model, &mut Vec::new(), &mut seen);
    let states = seen.len();
    assert!(states > 10_000, "{states} states is not the space the model claims");
}
