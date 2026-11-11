//! A pager whose objects a child of its own maps, started by its row's
//! `pager`: what a mapping of a paged object does that only a booted kernel
//! has, its tables, its faults and a second process. A pager may not map,
//! so each case's mapper is this program again, `--map <case>`, endowed a
//! holder and pipe ends, and judged by its end and by what the pager hears.
//!
//! - `coherent`: a mapper sees the pager's write land in the page it maps,
//!   and its mapping holds the object after it closed its handle.
//! - `parks`: a touch of an absent page waits until it is supplied; a
//!   withdrawn page is asked for again and comes back with the same bytes;
//!   a supply held past the report time is reported overdue, and the mapper
//!   then goes on.
//! - `unmapped`: a range taken away under a thread parked in it ends the
//!   process as a touch of nothing once the page comes.
//! - `fault`, `copy`: a kill ends a thread parked in its own fault, and one
//!   parked in a write's copy from the mapping, which writes nothing.
//! - `hole`: an unwritten page of an anonymous object reads zero with no
//!   request, and one the pager has no room for ends the mapper.
//! - `shrunk`: an object of 80 pages, every one touched by two mappers, is
//!   shrunk to one; a touch of the first page past the new end ends one
//!   mapper and of the last the other, each with `SIGBUS`. The shrink revokes
//!   79 pages, more than a walk reads at once, so one of the two is cleared
//!   by a later batch.
//! - `ends`, last: the pager's end answers a write from the mapping `Io`
//!   and ends the mapper at its next touch.

use std::os::toyos::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use toyos::endow::Endowments;
use toyos::paged::{Notice, Paged, PagedNotice, Pager, PAGE};
use toyos::poller::{Poller, READABLE};
use toyos::{OwnedHandle, Pipe};
use toyos_abi::end::{End, Fault};
use toyos_abi::syscall::{self, SyscallError, VmAt};

const SELF: &str = "/system/bin/mapprobe";
const HOLDER: &str = "mapprobe-holder";
const READY: &str = "mapprobe-ready";
const GO: &str = "mapprobe-go";
const OUT: &str = "mapprobe-out";

/// Every page the pager holds at once.
const CAPACITY: u64 = 256;
/// The pager's report time, which `parks` holds a supply past.
const REPORT: Duration = Duration::from_secs(1);
/// How long a step may take before the probe gives up on it: a failure
/// bound, never what ends a passing run.
const BOUND: Duration = Duration::from_secs(60);

/// The aligned run a fault's request names, in pages.
const RUN: u64 = 16;

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--map") {
        return map(&args.next().expect("mapprobe --map: a case"));
    }
    let pager = Pager::create(CAPACITY, 0, REPORT).expect("mapprobe: its row starts it as a pager");
    let mut served = Served { pager, poller: Poller::new(1), unheld: Vec::new() };
    coherent(&mut served);
    parks(&mut served);
    killed(&mut served);
    hole(&mut served);
    shrunk(&mut served);
    ends(served);
    println!("mapprobe: ok");
}

/// The pager, and the objects whose last holder its notices said went.
struct Served {
    pager: Pager,
    poller: Poller,
    unheld: Vec<u64>,
}

impl Served {
    /// Wait for a request for `object` naming page `first`. A request for
    /// another run is a duplicate of one answered: the kernel names a run
    /// again after a withdraw meets its waiter.
    fn requested(&mut self, object: u64, first: u64) {
        let began = Instant::now();
        while !self.heard(|n| matches!(n, Notice::Requested { object: o, first: f, .. } if o == object && f == first)) {
            self.wait(began, &format!("a request for object {object} page {first}"));
        }
    }

    /// Wait for the notice that `object`'s last holder went.
    fn unheld(&mut self, object: u64) {
        let began = Instant::now();
        while !self.unheld.contains(&object) {
            self.heard(|_| false);
            if !self.unheld.contains(&object) {
                self.wait(began, &format!("object {object}'s last holder to go"));
            }
        }
    }

    /// Whether a pending notice is one `want` names, the rest read and kept.
    fn heard(&mut self, want: impl Fn(Notice) -> bool) -> bool {
        let mut buf = [PagedNotice::default(); 8];
        let mut found = false;
        for notice in self.pager.notices(&mut buf).expect("mapprobe: its notices") {
            if let Notice::NoHolders { object } = notice {
                self.unheld.push(object);
            }
            found |= want(notice);
        }
        found
    }

    fn wait(&self, began: Instant, what: &str) {
        let left = BOUND.checked_sub(began.elapsed()).unwrap_or_else(|| panic!("mapprobe: no {what} within {BOUND:?}"));
        self.poller.watch(&self.pager, READABLE, 0);
        self.poller.wait(1, left.as_nanos() as u64, |_| {});
    }

    fn supply(&self, object: u64, first: u64, count: u64, byte: u8) {
        let bytes = vec![byte; (count * PAGE) as usize];
        // A page already present was a duplicate's: the kernel installs nothing.
        self.pager.supply(object, first * PAGE, &bytes, false).expect("mapprobe: a supply");
    }

    /// A mapper of `object` for `case`, endowed a holder and `ends`.
    fn mapper(&self, object: u64, case: &str, ends: Vec<(&str, OwnedHandle)>) -> Child {
        let holder = self.pager.holder(object).expect("mapprobe: a holder to hand on");
        let mut command = Command::new(SELF);
        command.arg("--map").arg(case).endow(HOLDER, holder.into());
        for (label, end) in ends {
            command.endow(label, end.into());
        }
        command.spawn().expect("mapprobe: a mapper starts")
    }
}

/// A pipe: the pager's end, and the mapper's.
fn pipe() -> (Pipe, OwnedHandle) {
    let (read, write) = toyos::pipe_pair().expect("mapprobe: a pipe");
    (read, OwnedHandle::from(write))
}

/// A pipe the other way: the mapper reads.
fn go_pipe() -> (Pipe, OwnedHandle) {
    let (read, write) = toyos::pipe_pair().expect("mapprobe: a pipe");
    (write, OwnedHandle::from(read))
}

fn ended(child: &mut Child) -> End {
    End::from_word(child.wait().expect("mapprobe: a mapper's end").into_raw()).expect("mapprobe: an end the kernel makes")
}

fn coherent(served: &mut Served) {
    let object = served.pager.object(2 * PAGE).expect("mapprobe: an object");
    served.supply(object, 0, 1, 0x11);
    let (ready, theirs) = pipe();
    let mut child = served.mapper(object, "coherent", vec![(READY, theirs)]);
    assert_eq!(ready.read(&mut [0]), Ok(1), "mapprobe: the coherent mapper never read the supplied page");
    assert!(!served.heard(|n| n == Notice::NoHolders { object }), "mapprobe: a mapped object's last holder was said to go");
    assert_eq!(served.pager.write(object, 0, &[0x22; PAGE as usize]), Ok(PAGE as usize));
    assert_eq!(ended(&mut child), End::Exited(0), "mapprobe: the coherent mapper");
    served.unheld(object);
    served.pager.release(object).expect("mapprobe: release");
    println!("mapprobe: a mapper sees the pager's write, and its mapping holds the object it closed");
}

fn parks(served: &mut Served) {
    let object = served.pager.object(4 * RUN * PAGE).expect("mapprobe: an object");
    let mut child = served.mapper(object, "parks", Vec::new());
    served.requested(object, 0);
    served.supply(object, 0, RUN, 0x33);
    served.requested(object, RUN);
    assert_eq!(served.pager.withdraw(object, 0, RUN), Ok(RUN), "mapprobe: the first run withdrawn under its mapper");
    // A stall is the stimulus here, not a wait for an event: the mapper's
    // fault waits past the report time while the pager does nothing.
    std::thread::sleep(2 * REPORT);
    served.supply(object, RUN, RUN, 0x44);
    served.requested(object, 0);
    served.supply(object, 0, RUN, 0x33);
    assert_eq!(ended(&mut child), End::Exited(0), "mapprobe: the parked mapper");
    println!("mapprobe: a fault waits for its page, a withdrawn page comes back, and a stalled supply is waited out");

    let (ready, theirs) = pipe();
    let (go, mine) = go_pipe();
    let mut child = served.mapper(object, "unmapped", vec![(READY, theirs), (GO, mine)]);
    served.requested(object, 3 * RUN);
    assert_eq!(go.write(&[1]), Ok(1));
    assert_eq!(ready.read(&mut [0]), Ok(1), "mapprobe: the mapper never took its range away");
    served.supply(object, 3 * RUN, RUN, 0x55);
    assert_eq!(ended(&mut child), End::Fault(Fault::Memory), "mapprobe: a thread woken in a range taken away");
    served.pager.release(object).expect("mapprobe: release");
    println!("mapprobe: a range taken away under a parked thread is nothing when its page comes");
}

fn killed(served: &mut Served) {
    let object = served.pager.object(2 * RUN * PAGE).expect("mapprobe: an object");
    let mut child = served.mapper(object, "fault", Vec::new());
    served.requested(object, 0);
    child.kill().expect("mapprobe: kill the parked mapper");
    assert_eq!(ended(&mut child), End::Killed, "mapprobe: a mapper parked in its fault");
    let (out, theirs) = pipe();
    let mut child = served.mapper(object, "copy", vec![(OUT, theirs)]);
    served.requested(object, RUN);
    child.kill().expect("mapprobe: kill the parked writer");
    assert_eq!(ended(&mut child), End::Killed, "mapprobe: a mapper parked in a write's copy");
    assert_eq!(out.read(&mut [0; 16]), Ok(0), "mapprobe: a write killed in its copy wrote");
    served.pager.release(object).expect("mapprobe: release");
    println!("mapprobe: a kill ends a thread parked in its fault or in a copy, and the copy's write wrote nothing");
}

fn hole(served: &mut Served) {
    let object = served.pager.anonymous(2 * PAGE).expect("mapprobe: anonymous memory");
    let room = served.pager.room().expect("mapprobe: room");
    let (ready, theirs) = pipe();
    let (go, mine) = go_pipe();
    let mut child = served.mapper(object, "hole", vec![(READY, theirs), (GO, mine)]);
    assert_eq!(ready.read(&mut [0]), Ok(1), "mapprobe: the mapper never read its hole");
    assert!(!served.heard(|n| matches!(n, Notice::Requested { object: o, .. } if o == object)), "mapprobe: a hole was requested");
    assert_eq!(served.pager.room(), Ok(room - 1), "mapprobe: a touched hole is not one page of the pager's");
    let filler = served.pager.object((room - 1) * PAGE).expect("mapprobe: an object to fill the pager");
    served.supply(filler, 0, room - 1, 0x99);
    assert_eq!(served.pager.room(), Ok(0));
    assert_eq!(go.write(&[1]), Ok(1));
    assert_eq!(ended(&mut child), End::Fault(Fault::Mapping), "mapprobe: a hole touched with no room");
    served.pager.release(filler).expect("mapprobe: release");
    served.pager.release(object).expect("mapprobe: release");
    println!("mapprobe: a touched hole is a zeroed page of the pager's, and none where it has no room");
}

/// The pages `shrunk`'s object holds before its shrink: more than a walk's
/// batch, so its revocation takes two.
const SHRUNK: u64 = 80;

fn shrunk(served: &mut Served) {
    let object = served.pager.object(SHRUNK * PAGE).expect("mapprobe: an object");
    served.supply(object, 0, SHRUNK, 0x66);
    let mut mappers = Vec::new();
    for page in [1, SHRUNK - 1] {
        let (ready, theirs) = pipe();
        let (go, mine) = go_pipe();
        let child = served.mapper(object, &format!("shrunk:{page}"), vec![(READY, theirs), (GO, mine)]);
        assert_eq!(ready.read(&mut [0]), Ok(1), "mapprobe: a mapper never read every page");
        mappers.push((page, child, go));
    }
    served.pager.resize(object, PAGE).expect("mapprobe: a shrink");
    for (page, mut child, go) in mappers {
        assert_eq!(go.write(&[1]), Ok(1));
        assert_eq!(ended(&mut child), End::Fault(Fault::Mapping), "mapprobe: a touch of page {page} past a shrunk object");
    }
    served.pager.release(object).expect("mapprobe: release");
    println!("mapprobe: a touch of a page the object shrank past ends its mapper as SIGBUS");
}

fn ends(mut served: Served) {
    let object = served.pager.object(2 * PAGE).expect("mapprobe: an object");
    served.supply(object, 0, 1, 0x77);
    let (out, theirs) = pipe();
    let mut child = served.mapper(object, "ends", vec![(OUT, theirs)]);
    served.requested(object, 0);
    drop(served);
    assert_eq!(ended(&mut child), End::Fault(Fault::Mapping), "mapprobe: a mapper of an ended pager's object");
    assert_eq!(out.read(&mut [0; 16]), Ok(0), "mapprobe: a write from an ended pager's page wrote");
    println!("mapprobe: a pager's end answers a write from its page Io, and ends its mapper at the next touch");
}

/// The object a mapper was endowed, mapped whole.
fn mapped(pages: u64) -> *mut u8 {
    let handle: OwnedHandle = Endowments::get().take(HOLDER).expect("mapprobe --map: its holder");
    let holder = Paged::received(handle).expect("mapprobe --map: a holder it may read");
    // SAFETY: placed anywhere, so it replaces nothing; the holder goes and
    // the mapping holds the object.
    unsafe { holder.map(0, (pages * PAGE) as usize, VmAt::Anywhere { align: PAGE as usize }) }.expect("mapprobe --map: the object mapped")
}

fn byte(at: *mut u8, page: u64) -> u8 {
    // SAFETY: inside the mapping this case made, read-only.
    unsafe { at.add((page * PAGE) as usize).read_volatile() }
}

fn pipe_end(label: &str) -> Pipe {
    Endowments::get().take(label).expect("mapprobe --map: a pipe end")
}

fn map(case: &str) {
    match case {
        "coherent" => {
            let at = mapped(2);
            assert_eq!(byte(at, 0), 0x11, "mapprobe --map: the supplied page");
            assert_eq!(pipe_end(READY).write(&[1]), Ok(1));
            let began = Instant::now();
            while byte(at, 0) != 0x22 {
                assert!(began.elapsed() < BOUND, "mapprobe --map: the pager's write never reached the mapping");
                std::thread::yield_now();
            }
        }
        "parks" => {
            let at = mapped(4 * RUN);
            assert_eq!(byte(at, 4), 0x33);
            assert_eq!(byte(at, RUN + 4), 0x44);
            assert_eq!(byte(at, 4), 0x33, "mapprobe --map: a withdrawn page came back as other bytes");
        }
        "unmapped" => {
            let at = mapped(4 * RUN) as usize;
            let parked = std::thread::spawn(move || byte(at as *mut u8, 3 * RUN + 2));
            assert_eq!(pipe_end(GO).read(&mut [0]), Ok(1));
            // SAFETY: the mapping this case made, which the parked thread
            // touches again only to fault.
            unsafe { syscall::vm_release(at as *mut u8, (4 * RUN * PAGE) as usize) }.expect("mapprobe --map: unmap");
            assert_eq!(pipe_end(READY).write(&[1]), Ok(1));
            let read = parked.join();
            panic!("mapprobe --map: a thread woken in a range taken away read {read:?}");
        }
        "fault" => {
            let read = byte(mapped(2 * RUN), 2);
            panic!("mapprobe --map: a page nobody supplies read {read}");
        }
        "copy" => {
            let at = mapped(2 * RUN);
            // SAFETY: a page of the mapping, which the kernel copies from.
            let page = unsafe { std::slice::from_raw_parts(at.add((RUN * PAGE) as usize), PAGE as usize) };
            let wrote = pipe_end(OUT).write(page);
            panic!("mapprobe --map: a write from a page nobody supplies answered {wrote:?}");
        }
        "hole" => {
            let at = mapped(2);
            assert_eq!(byte(at, 0), 0, "mapprobe --map: an unwritten page");
            assert_eq!(pipe_end(READY).write(&[1]), Ok(1));
            assert_eq!(pipe_end(GO).read(&mut [0]), Ok(1));
            let read = byte(at, 1);
            panic!("mapprobe --map: a hole the pager had no room for read {read}");
        }
        shrunk if shrunk.starts_with("shrunk:") => {
            let page: u64 = shrunk["shrunk:".len()..].parse().expect("mapprobe --map: a page");
            let at = mapped(SHRUNK);
            assert!((0..SHRUNK).all(|page| byte(at, page) == 0x66), "mapprobe --map: the supplied pages");
            assert_eq!(pipe_end(READY).write(&[1]), Ok(1));
            assert_eq!(pipe_end(GO).read(&mut [0]), Ok(1));
            let read = byte(at, page);
            panic!("mapprobe --map: page {page}, past the object's end, read {read}");
        }
        "ends" => {
            let at = mapped(2);
            assert_eq!(byte(at, 0), 0x77);
            // SAFETY: a page of the mapping, which the kernel copies from.
            let page = unsafe { std::slice::from_raw_parts(at.add(PAGE as usize), PAGE as usize) };
            assert_eq!(pipe_end(OUT).write(page), Err(SyscallError::Io), "mapprobe --map: a write from an ended pager's page");
            let read = byte(at, 0);
            panic!("mapprobe --map: a page of an ended pager read {read}");
        }
        other => panic!("mapprobe --map: no case {other:?}"),
    }
}
