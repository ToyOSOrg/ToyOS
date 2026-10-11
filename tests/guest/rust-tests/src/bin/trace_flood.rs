//! A flood longer than a ring: the records it overwrote before the reader got
//! there are counted, and the ones the reader gets are the newest, whole and in
//! order. On the T14 its line is what one record costs.
//!
//! **A guest on the test kernel**, because nothing else writes a known
//! sequence of records into one CPU's ring faster than a reader drains it:
//! `SYS_DEBUG`'s `TRACE_FLOOD` writes them through the shipped writer, and the
//! read, its loss count and its cursor are the shipped paths. The ticks it
//! answers are printed and judged on the T14 alone (`trace_record_cost`).

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::syscall::{self, debug_action, SyscallError};
use toyos_abi::trace::{TraceCursor, TraceRecord};
use toyos_trace::{Entry, Event};

/// A million: the count the track's exit measures a record's cost over.
const COUNT: u64 = 1_000_000;

/// Records asked for at once.
const BATCH: usize = 512;

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a capability");
    assert_eq!(
        syscall::debug_with(debug_action::TRACE_FLOOD, debug_action::TRACE_FLOOD_MOST + 1),
        SyscallError::InvalidArgument.to_u64(),
        "a flood past the most one call writes was written",
    );
    let mut cursor = TraceCursor::new();
    read_to_now(&cap, &mut cursor);

    let ticks = syscall::debug_with(debug_action::TRACE_FLOOD, COUNT);
    assert!(SyscallError::from_u64(ticks).is_none(), "the flood was refused: {ticks:#x}");
    let (entries, lost) = read_to_now(&cap, &mut cursor);

    let marks: Vec<(u16, u32)> = entries
        .iter()
        .filter_map(|e| match e.event {
            Event::Mark { index } => Some((e.cpu, index)),
            _ => None,
        })
        .collect();
    let (&(cpu, oldest), &(_, newest)) =
        marks.first().zip(marks.last()).unwrap_or_else(|| panic!("no record of the flood was read back"));
    assert!(marks.iter().all(|&(c, _)| c == cpu), "the flood's records came from more than its own cpu{cpu}");
    assert!(
        marks.windows(2).all(|w| w[1].1 == w[0].1 + 1),
        "the flood's records read back out of order or with a gap, from {oldest} to {newest}",
    );
    assert_eq!(u64::from(newest), COUNT - 1, "the flood's newest record was not read back");
    assert!(oldest > 0, "a flood of {COUNT} read back whole: no ring is that long");
    assert!(lost >= u64::from(oldest), "{oldest} of the flood's records were overwritten and {lost} counted lost");

    println!(
        "trace_flood: {COUNT} records in {ticks} counter ticks with interrupts closed, {:.2} a record; \
         the newest {} read back whole, {lost} counted lost",
        ticks as f64 / COUNT as f64,
        marks.len(),
    );
}

/// Every record from `cursor` on, decoded, until a read leaves its buffer
/// short, and the records lost on the way.
fn read_to_now(cap: &SysCap, cursor: &mut TraceCursor) -> (Vec<Entry>, u64) {
    let mut out = Vec::new();
    let mut lost = 0;
    let mut raw = vec![TraceRecord::EMPTY; BATCH];
    loop {
        let n = cap.trace(cursor, &mut raw).expect("the estate's capability reads the diary");
        lost += cursor.lost();
        out.extend(raw[..n].iter().map(|r| Entry::decode(r).unwrap_or_else(|why| panic!("{r:?}: {why}"))));
        if n < raw.len() {
            return (out, lost);
        }
    }
}
