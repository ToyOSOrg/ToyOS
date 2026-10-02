//! What `poll` asks a ring to watch: one watch per descriptor, which answers
//! every entry that names it.

use toyos_abi::inbox::{READABLE, WRITABLE};

use crate::header;
use crate::pollreq::{watch_of, watches};

const POLL_H: &str = include_str!("../../userland/libc/include/poll.h");

fn events(names: &[&str]) -> i16 {
    names.iter().fold(0, |events, name| events | header::int(POLL_H, name) as i16)
}

#[test]
fn a_descriptor_named_twice_is_watched_once_for_both_interests() {
    let entries = [(5, events(&["POLLIN"])), (7, events(&["POLLOUT"])), (5, events(&["POLLOUT"]))];
    assert_eq!(watches(&entries), [(0, 5, READABLE | WRITABLE), (1, 7, WRITABLE)]);
    assert_eq!([0, 1, 2].map(|entry| watch_of(&entries, entry)), [0, 1, 0]);
}

#[test]
fn each_descriptor_named_once_has_its_own_watch() {
    let entries = [(3, events(&["POLLIN", "POLLOUT"])), (4, events(&["POLLIN"])), (9, events(&["POLLHUP"]))];
    assert_eq!(watches(&entries), [(0, 3, READABLE | WRITABLE), (1, 4, READABLE), (2, 9, 0)]);
    assert_eq!([0, 1, 2].map(|entry| watch_of(&entries, entry)), [0, 1, 2]);
}
