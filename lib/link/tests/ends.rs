//! Both ends of a link over one header of words and an arena of bytes, as
//! the two processes share them, and every value a hostile peer can write.

use core::sync::atomic::{AtomicU32, Ordering};

use toyos::transport::{Span, Violation};
use toyos_link::ends::{Driver, Fault, Stack};
use toyos_link::layout::{self, REGION_BYTES, RX_DEPTH, SLOT_BYTES, TX_DEPTH, WORDS};
use toyos_link::state::{Counter, Counters, Group, Link};

struct Region {
    words: [AtomicU32; WORDS],
    bytes: Vec<u8>,
}

impl Region {
    fn new() -> Self {
        Self { words: core::array::from_fn(|_| AtomicU32::new(0)), bytes: vec![0; REGION_BYTES] }
    }

    fn write(&mut self, at: Span, frame: &[u8]) {
        self.bytes[at.offset..at.offset + frame.len()].copy_from_slice(frame);
    }

    fn read(&self, at: Span) -> Vec<u8> {
        self.bytes[at.offset..at.offset + at.len].to_vec()
    }

    fn poke(&self, word: usize, value: u32) {
        self.words[word].store(value, Ordering::Release);
    }
}

fn frame(n: u32, len: usize) -> Vec<u8> {
    (0..len).map(|i| (n as usize * 7 + i) as u8).collect()
}

/// A link a driver made and the stack opened.
fn opened() -> (Region, Driver, Stack) {
    let region = Region::new();
    let driver = Driver::new(&region.words);
    let stack = Stack::new(&region.words);
    (region, driver, stack)
}

/// Frames received come out at the stack whole and in order, across many
/// wraps of the ring, and a slot is the driver's again only once the stack
/// released it.
#[test]
fn received_frames_reach_the_stack_whole_and_in_order() {
    let (mut region, mut driver, mut stack) = opened();
    let (mut sent, mut taken) = (0u32, 0u32);
    for round in 0..5 * RX_DEPTH {
        for _ in 0..1 + round % 37 {
            if driver.room(&region.words).unwrap() == 0 {
                assert_eq!(sent - taken, RX_DEPTH, "no room with less than a ring unreleased");
                break;
            }
            let len = 1 + (sent as usize * 31) % SLOT_BYTES;
            let slot = driver.slot();
            region.write(slot, &frame(sent, len));
            driver.receive(&region.words, len).unwrap();
            sent += 1;
        }
        driver.publish(&region.words);
        for _ in 0..1 + round % 23 {
            let Some(at) = stack.received(&region.words).unwrap() else { break };
            let len = 1 + (taken as usize * 31) % SLOT_BYTES;
            assert_eq!(region.read(at), frame(taken, len), "frame {taken}");
            taken += 1;
        }
        stack.release(&region.words);
    }
    assert!(sent > 2 * RX_DEPTH, "the ring wrapped");
}

/// The same the other way, frames the stack sends.
#[test]
fn frames_sent_reach_the_driver_whole_and_in_order() {
    let (mut region, mut driver, mut stack) = opened();
    let (mut sent, mut taken) = (0u32, 0u32);
    for round in 0..5 * TX_DEPTH {
        for _ in 0..1 + round % 41 {
            if stack.room(&region.words).unwrap() == 0 {
                break;
            }
            let len = 60 + sent as usize % 1455;
            let slot = stack.slot();
            region.write(slot, &frame(sent, len));
            stack.send(&region.words, len).unwrap();
            sent += 1;
        }
        stack.publish(&region.words);
        for _ in 0..1 + round % 19 {
            let Some(at) = driver.to_send(&region.words).unwrap() else { break };
            assert_eq!(region.read(at), frame(taken, 60 + taken as usize % 1455));
            taken += 1;
        }
        driver.release(&region.words);
    }
    assert!(sent > 2 * TX_DEPTH);
}

/// A full ring's producer that asks is rung once the consumer releases, and
/// a release nobody waited for rings nobody.
#[test]
fn a_full_ring_is_rung_when_its_room_returns() {
    let (mut region, mut driver, mut stack) = opened();
    assert!(!driver.release(&region.words), "nobody waits");
    while stack.room(&region.words).unwrap() > 0 {
        let slot = stack.slot();
        region.write(slot, &[1; 60]);
        stack.send(&region.words, 60).unwrap();
    }
    assert!(stack.publish(&region.words));
    assert_eq!(stack.want_room(&region.words), Ok(0));
    assert!(driver.to_send(&region.words).unwrap().is_some());
    assert!(driver.release(&region.words), "the stack waits for this room");
    assert_eq!(stack.room(&region.words), Ok(1));
    assert!(!driver.release(&region.words), "the want was taken");

    while driver.room(&region.words).unwrap() > 0 {
        driver.receive(&region.words, 60).unwrap();
    }
    driver.publish(&region.words);
    assert_eq!(driver.want_room(&region.words), Ok(0));
    assert!(stack.received(&region.words).unwrap().is_some());
    assert!(stack.release(&region.words));
    assert!(!stack.release(&region.words));
}

/// A frame the driver says is empty, or longer than its slot, ends the
/// session by name; so does a tail past the ring.
#[test]
fn a_frame_no_slot_holds_is_a_fault() {
    for (len, fault) in [(0, Fault::Length(0)), (SLOT_BYTES as u32 + 1, Fault::Length(SLOT_BYTES as u32 + 1)), (u32::MAX, Fault::Length(u32::MAX))] {
        let (region, _, mut stack) = opened();
        region.poke(layout::RX_ENTRIES, len);
        region.poke(layout::RX_TAIL, 1);
        assert_eq!(stack.received(&region.words), Err(fault));
    }
    let (region, mut driver, _) = opened();
    region.poke(layout::TX_ENTRIES, SLOT_BYTES as u32 + 1);
    region.poke(layout::TX_TAIL, 1);
    assert_eq!(driver.to_send(&region.words), Err(Fault::Length(SLOT_BYTES as u32 + 1)));

    let (region, _, mut stack) = opened();
    region.poke(layout::RX_TAIL, RX_DEPTH + 1);
    assert_eq!(stack.received(&region.words), Err(Fault::Ring(Violation::TailPastDepth)));
    let (region, _, mut stack) = opened();
    region.poke(layout::TX_HEAD, 1);
    assert_eq!(stack.room(&region.words), Err(Fault::Ring(Violation::HeadPastTail)));
}

/// The link the driver says is the stack's; a word no driver writes is a
/// fault, not a guess.
#[test]
fn the_link_reads_as_the_driver_said_it() {
    let (region, driver, stack) = opened();
    assert_eq!(stack.link(&region.words), Ok(Link::Down), "a new link is down");
    let up = Link::Up { mbps: 1000, full_duplex: true };
    driver.set_link(&region.words, up);
    assert_eq!(stack.link(&region.words), Ok(up));
    region.poke(layout::LINK, 0xdead_beef);
    assert_eq!(stack.link(&region.words), Err(Fault::Link(0xdead_beef)));
}

#[test]
fn each_group_joined_reaches_the_driver_once() {
    let (region, mut driver, mut stack) = opened();
    let groups: Vec<Group> = (0..layout::MAX_GROUPS as u8).map(|i| Group::new([0x01, 0x00, 0x5e, 0, 0, i]).unwrap()).collect();
    assert_eq!(driver.joined(&region.words), Ok(None));
    for (i, group) in groups.iter().enumerate() {
        assert!(stack.join(&region.words, *group));
        if i % 2 == 1 {
            assert_eq!(driver.joined(&region.words), Ok(Some(groups[i - 1])));
            assert_eq!(driver.joined(&region.words), Ok(Some(*group)));
            assert_eq!(driver.joined(&region.words), Ok(None));
        }
    }
    assert!(!stack.join(&region.words, groups[0]), "the region holds no more groups");
}

/// A count past what the region holds, a count that went backwards, and a
/// group's words that are no group address are faults.
#[test]
fn groups_the_stack_never_joined_are_a_fault() {
    let (region, mut driver, _) = opened();
    region.poke(layout::GROUPS_JOINED, layout::MAX_GROUPS as u32 + 1);
    assert_eq!(driver.joined(&region.words), Err(Fault::Groups(layout::MAX_GROUPS as u32 + 1)));

    let (region, mut driver, mut stack) = opened();
    assert!(stack.join(&region.words, Group::new([0x33, 0x33, 0, 0, 0, 1]).unwrap()));
    assert!(driver.joined(&region.words).unwrap().is_some());
    region.poke(layout::GROUPS_JOINED, 0);
    assert_eq!(driver.joined(&region.words), Err(Fault::Groups(0)));

    let (region, mut driver, _) = opened();
    region.poke(layout::GROUPS, 0x0000_0052);
    region.poke(layout::GROUPS_JOINED, 1);
    assert_eq!(driver.joined(&region.words), Err(Fault::Group), "an individual address");
}

/// The stack reads counters only once the driver answered its latest ask,
/// whatever the driver wrote in them.
#[test]
fn counters_are_read_once_answered() {
    let (region, driver, mut stack) = opened();
    assert_eq!(driver.counters_asked(&region.words), None, "nothing asked");
    stack.ask_counters(&region.words);
    assert_eq!(stack.counters(&region.words), None, "not yet answered");
    let asked = driver.counters_asked(&region.words).expect("an ask");
    let mut counters = Counters::default();
    counters.set(Counter::WireSent, u64::MAX);
    counters.set(Counter::ErrorsCrc, 7);
    driver.answer(&region.words, asked, &counters);
    assert_eq!(driver.counters_asked(&region.words), None);
    assert_eq!(stack.counters(&region.words), Some(counters));

    stack.ask_counters(&region.words);
    assert_eq!(stack.counters(&region.words), None, "the last answer is the last ask's");
    region.poke(layout::COUNTERS_PRESENT, u32::MAX);
    region.poke(layout::COUNTERS_ANSWERED, asked + 1);
    let hostile = stack.counters(&region.words).expect("answered");
    assert_eq!(hostile.iter().count(), Counter::ALL.len(), "bits past the last counter name nothing");
}
