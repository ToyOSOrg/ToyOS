//! Calls beside the writer on two CPUs, under loom: every interleaving and
//! every memory order of a hold against a close, a reuse and a `dup2`. Each
//! case names the controls that red it.
//!
//!   cargo test -p toyos --lib --features <control> fd::table::model

use core::sync::atomic::Ordering;

use loom::sync::atomic::AtomicU32;
use loom::sync::{Arc, Mutex};
use loom::thread;

use super::{Error, Fault, Fd, Place, Slot, Table, Word, Writer};

struct Shared(AtomicU32);

impl Word for Shared {
    fn load(&self, order: Ordering) -> u32 {
        self.0.load(order)
    }
    fn store(&self, value: u32, order: Ordering) {
        self.0.store(value, order)
    }
    fn compare_exchange(&self, current: u32, new: u32, success: Ordering, failure: Ordering) -> Result<u32, u32> {
        self.0.compare_exchange(current, new, success, failure)
    }
    fn fetch_sub(&self, value: u32, order: Ordering) -> u32 {
        self.0.fetch_sub(value, order)
    }
}

/// A slot's object, which loom fails on a read beside a write or one not
/// ordered after it.
struct Tracked(loom::cell::UnsafeCell<usize>);

impl Place for Tracked {
    type Object = usize;

    unsafe fn read(&self) -> usize {
        // SAFETY: the caller's.
        self.0.with(|object| unsafe { *object })
    }

    unsafe fn write(&self, object: usize) {
        // SAFETY: the caller's.
        self.0.with_mut(|place| unsafe { *place = object });
    }
}

const N: usize = 2;
/// The objects a case installs, each named by its index.
const OBJECTS: usize = 3;

struct World {
    table: Table<Shared, Tracked, N>,
    writing: Mutex<()>,
    /// How often each object was let go.
    gone: [AtomicU32; OBJECTS],
}

impl World {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            table: Table {
                fds: core::array::from_fn(|_| Shared(AtomicU32::new(0))),
                slots: core::array::from_fn(|_| Slot {
                    refs: Shared(AtomicU32::new(0)),
                    object: Tracked(loom::cell::UnsafeCell::new(usize::MAX)),
                }),
            },
            writing: Mutex::new(()),
            gone: core::array::from_fn(|_| AtomicU32::new(0)),
        })
    }

    fn write<R>(&self, f: impl FnOnce(&mut Writer<'_, Shared, Tracked, N>) -> Result<R, Fault>) -> R {
        let _held = self.writing.lock().unwrap();
        // SAFETY: the one writer while `writing` is held.
        f(&mut unsafe { self.table.writer() }).expect("the writer was refused")
    }

    fn let_go(&self, object: usize) {
        assert_eq!(self.gone[object].fetch_add(1, Ordering::SeqCst), 0, "object {object} was let go twice");
    }

    fn released(&self, object: Option<usize>) {
        if let Some(object) = object {
            self.let_go(object);
        }
    }

    fn install(&self, object: usize, floor: Fd) -> Fd {
        self.write(|w| w.install(object, false, floor))
    }

    fn close(&self, fd: Fd) {
        self.released(self.write(|w| w.close(fd)));
    }

    /// A call's hold of `fd`: its token and object.
    fn hold(&self, fd: Fd) -> Option<(usize, usize)> {
        match self.table.acquire(fd, |object| self.let_go(object)) {
            Ok(held) => Some(held),
            Err(Fault::Refused(Error::BadFd)) => None,
            Err(fault) => panic!("a call answered {fault:?}"),
        }
    }

    /// The end of the call [`Self::hold`] answered, whose object was not let
    /// go under it.
    fn done(&self, (token, object): (usize, usize)) {
        assert_eq!(self.gone[object].load(Ordering::SeqCst), 0, "a call held object {object} after it was let go");
        // SAFETY: a token `hold` answered, given back once.
        self.released(unsafe { self.table.unuse(token) }.expect("a call's own token"));
    }

    /// One call on `fd`: the object it held.
    fn call(&self, fd: Fd) -> Option<usize> {
        let held = self.hold(fd)?;
        self.done(held);
        Some(held.1)
    }

    /// Every object in `objects` was let go once.
    fn all_gone(&self, objects: impl IntoIterator<Item = usize>) {
        for object in objects {
            assert_eq!(self.gone[object].load(Ordering::SeqCst), 1, "object {object} was not let go");
        }
    }
}

/// `fd-hold-revives` counts a call onto the slot the close freed, so the
/// call holds an object already let go, here and in the two cases below
/// that free a slot under a call.
#[test]
fn a_call_beside_a_close_holds_a_live_object_or_none() {
    loom::model(|| {
        let world = World::new();
        let fd = world.install(0, 0);
        let caller = thread::spawn({
            let world = world.clone();
            move || world.call(fd)
        });
        world.close(fd);
        caller.join().unwrap();
        world.all_gone([0]);
    });
}

/// Two calls give their references up beside each other and the close.
#[test]
fn two_calls_beside_a_close_let_the_object_go_once() {
    loom::model(|| {
        let world = World::new();
        let fd = world.install(0, 0);
        let held = world.hold(fd).expect("an open descriptor");
        let caller = thread::spawn({
            let world = world.clone();
            move || world.call(fd)
        });
        world.close(fd);
        world.done(held);
        caller.join().unwrap();
        world.all_gone([0]);
    });
}

/// The close frees the descriptor's slot and another descriptor's object
/// takes it. `fd-hold-unchecked` keeps a reference without looking at the
/// descriptor again, so the call holds an object its descriptor never named;
/// `fd-publish-relaxed` stores the new count unordered after the object, so
/// the call reads an object it need not see written, here and in the next
/// case.
#[test]
fn a_call_on_a_closed_descriptor_never_holds_the_object_that_took_its_slot() {
    loom::model(|| {
        let world = World::new();
        let fd = world.install(0, 0);
        let caller = thread::spawn({
            let world = world.clone();
            move || world.call(fd)
        });
        world.close(fd);
        let other = world.install(1, fd + 1);
        let held = caller.join().unwrap();
        assert!(matches!(held, None | Some(0)), "a call on a closed descriptor held {held:?}");
        world.close(other);
        world.all_gone([0, 1]);
    });
}

#[test]
fn a_call_on_a_reopened_descriptor_holds_the_old_object_or_the_new() {
    loom::model(|| {
        let world = World::new();
        let fd = world.install(0, 0);
        let caller = thread::spawn({
            let world = world.clone();
            move || world.call(fd)
        });
        world.close(fd);
        assert_eq!(world.install(1, 0), fd);
        caller.join().unwrap();
        world.close(fd);
        world.all_gone([0, 1]);
    });
}

/// A descriptor is open throughout a `dup2` onto it.
#[test]
fn a_call_beside_a_dup2_holds_the_old_object_or_the_new() {
    loom::model(|| {
        let world = World::new();
        let old = world.install(0, 0);
        let new = world.install(1, 0);
        let caller = thread::spawn({
            let world = world.clone();
            move || world.call(new)
        });
        world.released(world.write(|w| w.dup2(old, new, false)));
        let held = caller.join().unwrap();
        assert!(matches!(held, Some(0 | 1)), "a call beside a dup2 held {held:?}");
        world.all_gone([1]);
        world.close(old);
        world.close(new);
        world.all_gone([0]);
    });
}
