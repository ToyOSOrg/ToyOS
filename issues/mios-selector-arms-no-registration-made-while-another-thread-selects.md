---
status: open
kind: defect
severity: [hang]
opened: 2026-11-11
---

# mio's selector arms no registration made while another thread selects

The mio fork's ToyOS selector (`src/sys/toyos/selector.rs`) arms a watch for
each registration at the start of every `select`, from a copy of the list it
takes under the selector's lock, and then parks in `inbox_submit`. A
registration that another thread makes while that `select` is parked is not in
the copy. Its handle gets no watch until the parked `select` returns for some
other reason. With no timeout and nothing else ready, the `select` waits for
good, while the new source's event happens and is never reported. Upstream
mio's `Registry::try_clone` documents a registry that may be cloned to other
threads and registers with the same `Poll`, which is the case this misses.

The registering thread cannot arm the watch itself: a ring has one submitter,
so its `inbox_submit` while the `select` is parked is refused `Busy`.

Found by reading, not exercised.

**Exit**: a registration made while another thread is parked in `select` is
watched before that `select` would otherwise return, for example by the
registration waking the selector so that it arms the watch again; a test
registers a source from a second thread while the first is parked in a
`select` with no timeout, makes the source ready, and the `select` returns its
event.
