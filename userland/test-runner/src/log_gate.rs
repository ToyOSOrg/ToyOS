//! The conservation law, read through `SYS_LOG_READ` from inside `test-runner`.
//!
//! **It runs here rather than in a binary of its own, and that is capability
//! doctrine rather than convenience.** `test-runner` passes its whole
//! *namespace* to every binary it spawns, and `logread` is not a namespace
//! entry — it is a `SysCap` dup, exactly like `realtime`, which the estate does
//! not hand down either. So the gate that reads the machine's log is the one
//! process in a test image that holds the right from its own manifest row.
//!
//! **The verdict is exact, not statistical.** Every sequence number a shard
//! ever issued is either a record this reader took or one the kernel counted as
//! lost; no number is taken twice; and every storm record's text regenerates
//! byte for byte from the two numbers it declares. A torn record fails the
//! text, a lost record that is not counted fails the ledger, and a duplicated
//! one fails it the other way.
//!
//! **The storm is a thread of this process**, calling `SYS_DEBUG`'s
//! `LOG_PATTERNED` once per record and counting each call after it returns.
//! That counter, not any record, is what says the storm is over and which
//! records were read while it ran.
//!
//! **Nothing this reader waits for is a record the ring may drop.** The
//! termination condition is the *cursor*: the log has been drained and nothing
//! new has arrived for [`QUIET_READS`] reads, once the producer has returned
//! from its last call. The nesting burst's own `done` is a cross-check where it
//! survived and is never waited on. **The rule this shape exists to keep is
//! general**: a workload whose liveness depends on a record the ring is allowed
//! to drop is the same mistake wherever it appears.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use toyos::log::{LogTail, Record, MAX_LOG_SHARDS};
use toyos::poller::{Poller, READABLE};
use toyos::syscap::SysCap;
use toyos_abi::syscall::debug_action::LOG_PATTERNED;

/// The first sequence number any shard issues — one, so a slot nothing has ever
/// written cannot read as record 0 of every shard on every boot.
/// `kernel/src/log/shard.rs`'s `FIRST_SEQ` is the other half of this constant.
const FIRST_SEQ: u64 = 1;

/// Records per `SYS_LOG_READ`. Above the shard count, which the call refuses
/// below.
const BATCH: usize = 64;

/// Empty reads in a row before the log is called quiet.
///
/// **Eight, each after a bounded park on the readiness source**, because a
/// single empty read can land while a producer is inside its publication
/// bracket: `drain_ordered` stops that shard and says nothing about it, so a
/// ledger closed on the first empty read can be short by what was in flight.
const QUIET_READS: u32 = 8;

/// How long a park on the log's readiness source waits before giving up on it.
///
/// It is the gate's pacing as much as its wait: with nothing left to say the
/// kernel posts nothing, and eight of these is the whole tail of the run.
const IDLE_NANOS: u64 = 2_000_000;

/// How long the deterministic readiness round waits for its own record.
///
/// Generous, because what it bounds is a scheduler getting round to a child's
/// exit — not the post, which is one function call after the drain. A gate
/// that timed out here would be reporting the host's load and not the kernel's.
const READINESS_WAIT_NANOS: u64 = 2_000_000_000;

/// The poll's token. One handle is watched, so it identifies the round rather
/// than the source.
const LOG_TOKEN: u64 = 1;

/// `kernel/src/log/storm.rs`'s `PAYLOAD`.
const PAYLOAD: usize = 96;

/// The producer id `LOG_PATTERNED`'s records declare.
const STORM_PRODUCER: u64 = 0;

/// Records the storm emits: past a shard's 512, so the ring's drop-oldest path
/// is reachable.
const STORM_RECORDS: u64 = 1024;

/// `kernel/src/log/nested.rs`'s `NEST_PRODUCER`: the burst an interrupt handler
/// emits declares itself as this, so it goes through the same per-producer
/// ledger and the same byte-for-byte regeneration as a storm's records.
const NEST_PRODUCER: u64 = u64::MAX;

/// One producer's ledger.
#[derive(Default)]
struct Producer {
    /// The next index expected from this producer, and `None` before its first
    /// record.
    next: Option<u64>,
    read: u64,
}

/// One shard's ledger: the sequence numbers the kernel issued on that CPU.
#[derive(Default, Clone, Copy)]
struct ShardLedger {
    first: Option<u64>,
    next: u64,
    read: u64,
    /// Sequence numbers this reader never saw, derived from the gaps between
    /// the ones it did.
    gaps: u64,
    last_at_ns: u64,
}

/// The gate over whatever the boot's actuators write.
pub fn run(cap: Option<&SysCap>) -> i32 {
    report(cap, false)
}

/// The gate with a storm beside it.
pub fn run_storm(cap: Option<&SysCap>) -> i32 {
    report(cap, true)
}

fn report(cap: Option<&SysCap>, storm: bool) -> i32 {
    let Some(cap) = cap else {
        println!("log-gate: this program holds no system capability, so it holds no `logread`");
        return 1;
    };
    match gate(cap, storm) {
        Ok(()) => 0,
        Err(e) => {
            println!("log-gate: FAILED: {e}");
            1
        }
    }
}

struct Run {
    shards: [ShardLedger; MAX_LOG_SHARDS],
    producers: BTreeMap<u64, Producer>,
    /// The nesting gate's declared burst, once its `done` has been read. A
    /// cross-check and never a requirement: the burst laps its shard, so the
    /// ring is allowed to drop it.
    nest: Option<u64>,
    records: u64,
    reads: u64,
    /// Storm records taken by a read after which the producer had not yet
    /// returned from its last call. **Zero would mean this reader raced
    /// nothing**, which is the one way a green conservation law says nothing
    /// at all.
    concurrent: u64,
    /// Times the log's readiness source completed a poll.
    completions: u64,
}

fn gate(cap: &SysCap, storm: bool) -> Result<(), String> {
    let mut tail = LogTail::new();
    let mut buf = [Record::EMPTY; BATCH];
    let mut run = Run {
        shards: [ShardLedger::default(); MAX_LOG_SHARDS],
        producers: BTreeMap::new(),
        nest: None,
        records: 0,
        reads: 0,
        concurrent: 0,
        completions: 0,
    };

    // **Armed before the storm starts and kept armed**, which is what makes a
    // completion deterministic rather than lucky: the storm's records are
    // committed after this poll was registered, and re-arming after every
    // harvest means a post landing *during* the storm finds a pending poll
    // rather than a gap.
    //
    // **It used to arm only on an empty read, and that made the assertion
    // depend on the shape of the boot.** During a storm no read is empty, so the
    // only poll in flight was the one from before the first read; whether it was
    // ever completed came down to when `klogd` happened to get a turn. At
    // `--smp 4` that measured `wakes=1`, and at `--smp 8` with `/system/bin/logd` also
    // reading the cursor it measured **zero** — a red about scheduling rather
    // than about the readiness source. `min_complete` 0 with no timeout submits
    // and harvests without blocking, so this costs one syscall a round.
    let poller = Poller::new(1);
    poller.watch(cap, READABLE, LOG_TOKEN);
    let mut armed = true;

    let produced = Arc::new(AtomicU64::new(0));
    let mut producer = storm.then(|| spawn_producer(Arc::clone(&produced)));
    let target = if storm { STORM_RECORDS } else { 0 };

    let mut quiet = 0u32;
    loop {
        if !armed {
            poller.watch(cap, READABLE, LOG_TOKEN);
            armed = true;
        }
        poller.wait(0, 0, |token| {
            assert_eq!(token, LOG_TOKEN, "the log poll completed with another token");
            run.completions += 1;
            armed = false;
        });

        let batch = tail
            .read(cap, &mut buf)
            .map_err(|e| format!("SYS_LOG_READ refused a {BATCH}-record buffer: {e:?}"))?;
        // Loaded after the read returned: below `target` here is below it for the whole read.
        let during = produced.load(Ordering::Acquire);
        run.reads += 1;
        if batch.is_empty() {
            quiet += 1;
        } else {
            quiet = 0;
            run.records += batch.len() as u64;
        }

        let storm_before = storm_read(&run);
        for record in batch {
            account(record, &mut run)?;
        }
        if during < target {
            run.concurrent += storm_read(&run) - storm_before;
        }

        let finished = produced.load(Ordering::Acquire) == target;
        if quiet >= QUIET_READS && finished {
            break;
        }
        // A producer that returned short of `target` said why; waiting out the host's ceiling would lose it.
        if !finished && producer.as_ref().is_some_and(JoinHandle::is_finished) {
            join(producer.take())?;
        }
        if batch.is_empty() {
            // **Nothing new, so park on the readiness source rather than spin.**
            // `SYS_LOG_READ` never blocks by design; this is the other half of
            // that design, and the timeout is what bounds a machine that has
            // nothing left to say. The poll is already armed by the top of the
            // loop, so this parks on it rather than adding a second.
            poller.wait(1, IDLE_NANOS, |token| {
                assert_eq!(token, LOG_TOKEN, "the log poll completed with another token");
                run.completions += 1;
                armed = false;
            });
        }
    }
    join(producer.take())?;

    // **The readiness source, observed deterministically rather than raced.**
    // If the reads above completed no poll, make one: a child that runs and
    // exits commits `process.rs`'s `exit:` line, which is one kernel record
    // from userland with no actuator and no privilege behind it.
    if run.completions == 0 {
        let mut child = std::process::Command::new("/system/bin/echo")
            .arg("log-gate")
            .spawn()
            .map_err(|e| format!("the record-making child would not start: {e}"))?;
        let _ = child.wait();
        if !armed {
            poller.watch(cap, READABLE, LOG_TOKEN);
        }
        poller.wait(1, READINESS_WAIT_NANOS, |token| {
            assert_eq!(token, LOG_TOKEN, "the log poll completed with another token");
            run.completions += 1;
        });
    }

    verdict(&tail, &run, storm)
}

/// The storm: one kernel record per call, counted after each call returns.
fn spawn_producer(produced: Arc<AtomicU64>) -> JoinHandle<Result<(), String>> {
    std::thread::spawn(move || {
        for index in 0..STORM_RECORDS {
            let answer = toyos_abi::syscall::debug_with(LOG_PATTERNED, index);
            if answer != 0 {
                return Err(format!(
                    "SYS_DEBUG LOG_PATTERNED answered {answer:#x} at index {index}"
                ));
            }
            produced.fetch_add(1, Ordering::Release);
        }
        Ok(())
    })
}

fn join(producer: Option<JoinHandle<Result<(), String>>>) -> Result<(), String> {
    match producer.map(JoinHandle::join) {
        None | Some(Ok(Ok(()))) => Ok(()),
        Some(Ok(Err(e))) => Err(e),
        Some(Err(_)) => Err("the producer thread panicked".into()),
    }
}

fn storm_read(run: &Run) -> u64 {
    run.producers.get(&STORM_PRODUCER).map_or(0, |p| p.read)
}

/// Put one record through both ledgers.
fn account(record: &Record, run: &mut Run) -> Result<(), String> {
    let cpu = record.cpu as usize;
    let ledger = run.shards.get_mut(cpu).ok_or_else(|| {
        format!("a record claims cpu{cpu}, past the ABI's {MAX_LOG_SHARDS} shards")
    })?;

    match ledger.first {
        None => ledger.first = Some(record.seq),
        Some(_) => {
            if record.seq < ledger.next {
                return Err(format!(
                    "cpu{cpu} answered seq {} after seq {}: a sequence number was read twice, \
                     or out of order, within one shard",
                    record.seq,
                    ledger.next - 1
                ));
            }
            ledger.gaps += record.seq - ledger.next;
        }
    }
    if record.at_ns < ledger.last_at_ns {
        return Err(format!(
            "cpu{cpu} seq {} is stamped {} ns, behind the {} ns of the record before it — within \
             a shard the sequence order is the timestamp order, and `emit` stamps inside the \
             same bracket it reserves in",
            record.seq, record.at_ns, ledger.last_at_ns
        ));
    }
    ledger.last_at_ns = record.at_ns;
    ledger.next = record.seq + 1;
    ledger.read += 1;

    let message = record.message();
    if message.len() != record.len as usize {
        return Err(format!(
            "cpu{cpu} seq {} declares {} message bytes and decodes to {}",
            record.seq,
            record.len,
            message.len()
        ));
    }

    if let Some(rest) = message.strip_prefix("lognest done ") {
        let emitted = rest
            .split_whitespace()
            .find_map(|w| w.strip_prefix("emitted="))
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or_else(|| format!("`lognest done` is unreadable: {rest}"))?;
        if run.nest.replace(emitted).is_some() {
            return Err("the nesting gate said `done` twice".into());
        }
        return Ok(());
    }
    if message.starts_with("lognest ") {
        // `start` and `outer`. Both are records like any other and the burst
        // laps the shard they are in, so both are *expected* to be dropped —
        // which is the ring's declared policy and not a loss of evidence.
        return Ok(());
    }
    let Some(rest) = message.strip_prefix("logstorm t=") else {
        // An ordinary kernel record. It is in the shard ledger above, which is
        // where the conservation law is computed; it declares nothing this gate
        // could regenerate.
        return Ok(());
    };

    let (thread, index) = parse_record(rest)?;
    if thread != STORM_PRODUCER && thread != NEST_PRODUCER {
        return Err(format!(
            "cpu{cpu} seq {} names producer t={thread}, which no gate runs",
            record.seq
        ));
    }
    let expected = storm_message(thread, index);
    if message != expected {
        return Err(format!(
            "cpu{cpu} seq {} is a torn or mixed storm record\n  read:     {message}\n  expected: {expected}",
            record.seq
        ));
    }
    let producer = run.producers.entry(thread).or_default();
    if let Some(next) = producer.next {
        if index < next {
            return Err(format!(
                "producer t={thread} answered index {index} after {}: one record's body was \
                 published under another record's sequence number",
                next - 1
            ));
        }
    }
    producer.next = Some(index + 1);
    producer.read += 1;
    Ok(())
}

/// The line a storm record carries, from the two numbers that identify it.
///
/// **The kernel builds this and the reader rebuilds it**, so a body half
/// overwritten by another generation fails on the byte that differs rather than
/// on a checksum that might not have covered it. `kernel/src/log/storm.rs` is
/// the other half; a disagreement between the two formulas reds loudly rather
/// than passing quietly.
fn storm_message(thread: u64, index: u64) -> String {
    let checksum = (thread.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ index.wrapping_mul(0xC2B2_AE3D_27D4_EB4F))
    .rotate_left(17);
    let payload: String = (0..PAYLOAD)
        .map(|offset| (b'a' + (checksum.wrapping_add(offset as u64) % 26) as u8) as char)
        .collect();
    format!("logstorm t={thread} i={index} k={checksum:016x} {payload}")
}

fn parse_record(rest: &str) -> Result<(u64, u64), String> {
    let mut words = rest.split_whitespace();
    let thread = words
        .next()
        .and_then(|w| w.parse::<u64>().ok())
        .ok_or_else(|| format!("a storm record names no thread: {rest}"))?;
    let index = words
        .next()
        .and_then(|w| w.strip_prefix("i="))
        .and_then(|w| w.parse::<u64>().ok())
        .ok_or_else(|| format!("a storm record names no index: {rest}"))?;
    Ok((thread, index))
}

/// The conservation law, and everything the gate prints for a reader of its
/// output.
fn verdict(tail: &LogTail, run: &Run, storm: bool) -> Result<(), String> {
    let seen: Vec<usize> =
        (0..MAX_LOG_SHARDS).filter(|&i| run.shards[i].first.is_some()).collect();
    if seen.is_empty() {
        return Err("no shard answered a single record".into());
    }
    if tail.shards() as usize != seen.len() {
        return Err(format!(
            "the kernel says this machine has {} shard(s) and {} answered a record",
            tail.shards(),
            seen.len()
        ));
    }

    // **`records_emitted == records_read + lost`, with the sequence numbers as
    // the ledger.** Every number a shard issued is either a record this reader
    // took or one it never saw, and the second is what the kernel derives
    // `lost` from — out of `head` and `next`, two numbers that have to be right
    // anyway, rather than out of a producer-side counter that could drift from
    // the ring.
    let mut computed = 0u64;
    for &i in &seen {
        let first = run.shards[i].first.expect("`seen` is the shards with a first record");
        computed += first - FIRST_SEQ + run.shards[i].gaps;
    }
    let reported = tail.lost();
    if computed != reported {
        let per_shard: Vec<String> = seen
            .iter()
            .map(|&i| {
                format!(
                    "cpu{i}: first={} last={} read={} gaps={}",
                    run.shards[i].first.unwrap_or(0),
                    run.shards[i].next.saturating_sub(1),
                    run.shards[i].read,
                    run.shards[i].gaps
                )
            })
            .collect();
        return Err(format!(
            "conservation failed: the sequence numbers say {computed} record(s) were never read \
             and the kernel counted {reported}\n  {}",
            per_shard.join("\n  ")
        ));
    }

    let read_total = storm_read(run);
    if storm {
        if read_total == 0 {
            return Err("the storm ran and this reader read none of it".into());
        }
        let next = run.producers.get(&STORM_PRODUCER).and_then(|p| p.next).unwrap_or(0);
        if next > STORM_RECORDS {
            return Err(format!(
                "the storm answered index {} of {STORM_RECORDS} emitted",
                next - 1
            ));
        }
        if run.concurrent == 0 {
            return Err(
                "every storm record was read after the producer had finished, so this reader \
                 raced nothing"
                    .into(),
            );
        }
        // The readiness source, asserted where it is reachable: the poll was
        // armed before the storm started, so the records that answer it were
        // committed after it was registered.
        if run.completions == 0 {
            return Err(
                "the log's readiness source completed no poll — not across the storm, and not on \
                 the record a child's exit commits afterwards either"
                    .into(),
            );
        }
    }

    if let Some(burst) = run.producers.get(&NEST_PRODUCER) {
        // The burst's own `done` is a cross-check where it survived, and the
        // ledger's own floor where it did not. The burst laps its shard by
        // construction, so a reader that required that record would be
        // requiring one the design says may go.
        let declared = match (run.nest, burst.next) {
            (Some(declared), _) => declared,
            (None, Some(next)) => next,
            (None, None) => {
                return Err("the nesting burst was seen and named no index".into())
            }
        };
        if burst.read == 0 {
            return Err("the nesting burst was injected and none of it was read".into());
        }
        if burst.next.is_some_and(|next| next > declared) {
            return Err(format!(
                "the nesting burst answered index {} of a declared {declared}",
                burst.next.unwrap_or(0) - 1
            ));
        }
        println!(
            "log-gate: nest declared={declared} read={} dropped={}",
            burst.read,
            declared - burst.read,
        );
    }

    println!(
        "log-gate: {} record(s) over {} read(s) from {} shard(s); lost={reported}, and the \
         sequence numbers say the same",
        run.records,
        run.reads,
        seen.len()
    );
    if storm {
        println!(
            "log-gate: storm emitted={STORM_RECORDS} read={read_total} dropped={} \
             concurrent={} wakes={}",
            STORM_RECORDS - read_total,
            run.concurrent,
            run.completions,
        );
    }
    println!("log-gate: OK");
    Ok(())
}
