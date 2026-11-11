//! A pager whose end is what a test watches, started by its row's `pager`,
//! and the holders' writes it is asked to take.
//!
//! As the pager: it makes an object it never supplies and a holder of it,
//! is refused a read through its own holder (no pager waits on a pager), and
//! starts itself again as a helper, `--hold`, endowed the holder and write
//! holders of anonymous memory and of an object granted a page of credit.
//! Once its notices say the helper dirtied the credited object, it reads back
//! as their owner what the helper wrote to both; once they say the helper
//! waits on the first object's first page, it lets its pager go, and exits as
//! the helper did.
//!
//! Before that, one object holds more than 512 MiB at once, past the 131,072
//! frames whose list a removal once grew past the kernel heap's largest
//! allocation: it is withdrawn whole, filled and released, a second release
//! refused, and filled again for the pager's end to take.
//!
//! As the helper: a pipe end sent as a holder is no holder, and asking leaves
//! the helper alive. Its writes through write holders land, are stated and
//! read back; one past a holder's reach, or through a holder minted to read,
//! is refused with nothing written. Last, a holder's read of a page nobody
//! supplies, which only the pager's end answers, `Gone`; it exits 0 on exactly
//! that.

use std::os::toyos::process::CommandExt;
use std::process::Command;
use std::time::{Duration, Instant};

use toyos::endow::Endowments;
use toyos::paged::{Notice, PagedNotice, Paged, Pager, Refused, PAGE, PAGED_HOLDER_END, PAGED_RANGE_PAGES};
use toyos::poller::{Poller, READABLE};
use toyos_abi::syscall::SyscallError;

const HOLDER: &str = "pagerprobe-holder";
const ANONYMOUS: &str = "pagerprobe-anonymous";
const WRITTEN: &str = "pagerprobe-written";
const CREDITED: &str = "pagerprobe-credited";

/// The pages the helper's writes install: two of the anonymous memory, one
/// of the credited object.
const WRITES: u64 = 3;

/// What the helper writes: a page, then an append past it.
const PAGE_BYTE: u8 = 0xa5;
const TAIL: [u8; 100] = [0x5a; 100];

/// Pages of the large object: more than 512 MiB.
const LARGE: u64 = 140_000;
const SELF: &str = "/system/bin/pagerprobe";

/// How long the helper may take to park before the probe gives up on it: a
/// failure bound, never what ends a passing run.
const PARK_BOUND: Duration = Duration::from_secs(30);

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--hold") {
        return hold();
    }
    let pager = Pager::create(LARGE + 16 + WRITES, 1, 5, Duration::from_secs(1)).expect("pagerprobe: its row starts it as a pager");
    dirty_supply_prefix(&pager);
    let large = pager.object(LARGE * PAGE).expect("pagerprobe: the large object");
    fill(&pager, large);
    assert_eq!(pager.withdraw(large, 0, u64::MAX), Ok(LARGE), "pagerprobe: the whole object withdrawn at once");
    fill(&pager, large);
    pager.release(large).expect("pagerprobe: the large object released");
    assert_eq!(
        pager.release(large),
        Err(Refused::Kernel(SyscallError::InvalidArgument)),
        "pagerprobe: a second release of one object was taken"
    );
    let left = pager.object(LARGE * PAGE).expect("pagerprobe: the object the pager's end takes");
    fill(&pager, left);
    println!("pagerprobe: more than 512 MiB in one object withdrawn, released and left for the end");
    let anonymous = pager.anonymous(32 * PAGE).expect("pagerprobe: anonymous memory");
    assert_eq!(pager.hold(anonymous), Ok(32 * PAGE), "the pager holds its writes, and is told the size");
    assert_eq!(pager.supply(anonymous, 0, &vec![0x37; 16 * PAGE as usize], true), Ok(16));
    pager.unhold(anonymous).unwrap();
    assert_eq!(pager.room(), Ok(WRITES), "the anonymous pages consumed the quota but the helper's");
    assert_eq!(pager.withdraw(anonymous, 0, 32), Ok(0), "anonymous bytes cannot be withdrawn");
    assert_eq!(pager.fail(anonymous, 0, 32), Err(Refused::Kernel(SyscallError::InvalidArgument)));
    let mut bytes = vec![0xff; 32 * PAGE as usize];
    // SAFETY: `bytes` is local and borrowed only by this read until it returns.
    let out = unsafe { toyos::shm::Bytes::from_raw(bytes.as_mut_ptr(), bytes.len()) };
    assert_eq!(pager.read(anonymous, out, 0), Ok(bytes.len()));
    anonymous_contents(&bytes);
    assert_eq!(pager.room(), Ok(WRITES), "a zero-hole read needs no quota");
    let object = pager.object(4096).expect("pagerprobe: an object");
    let own = pager.holder(object, false).expect("pagerprobe: a holder for itself");
    // The handle is a holder, and this process a pager: refused by the kernel.
    assert_eq!(
        Paged::received(own).err(),
        Some(toyos::paged::NotAHolder::Refused),
        "pagerprobe: a pager was let read a holder, so it could wait on a pager"
    );
    println!("pagerprobe: a pager is refused a holder's read");

    let theirs = pager.holder(object, false).expect("pagerprobe: a holder to hand on");
    let memory = pager.holder(anonymous, false).expect("pagerprobe: an anonymous holder");
    let written = pager.anonymous(0).expect("pagerprobe: anonymous memory to write");
    let credited = pager.object(0).expect("pagerprobe: an object to write against credit");
    assert_eq!(pager.grant(credited, 1), Ok(1), "pagerprobe: a page of credit granted");
    let mut helper = Command::new(SELF)
        .arg("--hold")
        .endow(HOLDER, theirs.into())
        .endow(ANONYMOUS, memory.into())
        .endow(WRITTEN, pager.holder(written, true).expect("pagerprobe: a write holder").into())
        .endow(CREDITED, pager.holder(credited, true).expect("pagerprobe: a credited write holder").into())
        .spawn()
        .expect("pagerprobe: the helper starts");

    // The helper's dirtying of the credited object, which comes after its
    // other writes, and its request, the notice that it waits.
    let poller = Poller::new(1);
    let began = Instant::now();
    let mut buf = [PagedNotice::default(); 4];
    let (mut dirtied, mut waits) = (false, false);
    while !(dirtied && waits) {
        for notice in pager.notices(&mut buf).expect("pagerprobe: its notices") {
            match notice {
                Notice::Dirtied { object: o } if o == credited && !dirtied => {
                    dirtied = true;
                    owner_reads(&pager, written, credited);
                }
                Notice::Requested { object: o, first: 0, .. } if o == object => waits = true,
                _ => {}
            }
        }
        if dirtied && waits {
            break;
        }
        let Some(left) = PARK_BOUND.checked_sub(began.elapsed()) else {
            panic!("pagerprobe: the helper never {}", if dirtied { "waited on the page" } else { "dirtied the credited object" });
        };
        poller.watch(&pager, READABLE, 0);
        poller.wait(1, left.as_nanos() as u64, |_| {});
    }
    println!("pagerprobe: the helper waits on a page nobody supplies; ending the pager");
    drop(pager);
    let status = helper.wait().expect("pagerprobe: the helper's end");
    assert!(status.success(), "pagerprobe: the helper ended {status:?}");
    println!("pagerprobe: ok");
}

/// What the helper's writes left, as the owner reads it: the size the pager is
/// told as it holds the writes, and the bytes.
fn owner_reads(pager: &Pager, written: u64, credited: u64) {
    let page = PAGE as usize;
    assert_eq!(pager.take_dirtied(credited), Ok(Some((0, 1))), "pagerprobe: the credited object's dirty page");
    assert_eq!(pager.hold(credited), Ok(PAGE), "pagerprobe: the credited object's size after the holder's write");
    let mut bytes = vec![0; page];
    // SAFETY: the local buffer is borrowed only by this read.
    let out = unsafe { toyos::shm::Bytes::from_raw(bytes.as_mut_ptr(), bytes.len()) };
    assert_eq!(pager.read(credited, out, 0), Ok(page));
    assert!(bytes.iter().all(|&b| b == PAGE_BYTE), "pagerprobe: the credited object holds what the holder wrote");
    pager.unhold(credited).unwrap();
    let size = PAGE + TAIL.len() as u64;
    assert_eq!(pager.hold(written), Ok(size), "pagerprobe: the anonymous memory's size after the holder's append");
    let mut bytes = vec![0; page + TAIL.len()];
    // SAFETY: the local buffer is borrowed only by this read.
    let out = unsafe { toyos::shm::Bytes::from_raw(bytes.as_mut_ptr(), bytes.len()) };
    assert_eq!(pager.read(written, out, 0), Ok(bytes.len()));
    assert!(bytes[..page].iter().all(|&b| b == PAGE_BYTE), "pagerprobe: the holder's page");
    assert_eq!(&bytes[page..], &TAIL, "pagerprobe: the holder's append");
    pager.unhold(written).unwrap();
    println!("pagerprobe: the owner reads back what the helper's holders wrote");
}

fn dirty_supply_prefix(pager: &Pager) {
    let object = pager.anonymous(4 * PAGE).unwrap();
    let old = vec![0x42; PAGE as usize];
    assert_eq!(pager.supply(object, PAGE, &old, true), Err(Refused::Kernel(SyscallError::InvalidArgument)), "a write without the hold");
    pager.hold(object).unwrap();
    assert_eq!(pager.supply(object, PAGE, &old, true), Ok(1));
    assert_eq!(pager.supply(object, 3 * PAGE, &old, true), Ok(1));
    let new = vec![0x75; 4 * PAGE as usize];
    assert_eq!(pager.supply(object, 0, &new, true), Ok(1), "dirty supply stops at the first present page");
    pager.unhold(object).unwrap();
    let mut bytes = vec![0; new.len()];
    // SAFETY: the local buffer is borrowed only by this read.
    let out = unsafe { toyos::shm::Bytes::from_raw(bytes.as_mut_ptr(), bytes.len()) };
    assert_eq!(pager.read(object, out, 0), Ok(bytes.len()));
    assert_eq!(&bytes[..PAGE as usize], &new[..PAGE as usize]);
    assert_eq!(&bytes[PAGE as usize..2 * PAGE as usize], &old);
    assert!(bytes[2 * PAGE as usize..3 * PAGE as usize].iter().all(|&b| b == 0), "nothing past the returned prefix was installed");
    assert_eq!(&bytes[3 * PAGE as usize..], &old);
    pager.release(object).unwrap();
}

/// Every page of `object`, clean.
fn fill(pager: &Pager, object: u64) {
    let run = vec![0x5a_u8; (PAGED_RANGE_PAGES * PAGE) as usize];
    let mut at = 0;
    while at < LARGE {
        let pages = (LARGE - at).min(PAGED_RANGE_PAGES);
        let installed = pager.supply(object, at * PAGE, &run[..(pages * PAGE) as usize], false);
        assert_eq!(installed, Ok(pages), "pagerprobe: pages {at}+{pages} supplied");
        at += pages;
    }
}

fn hold() {
    let memory: toyos::OwnedHandle = Endowments::get().take(ANONYMOUS).expect("pagerprobe --hold: anonymous memory");
    let memory = Paged::received(memory).expect("pagerprobe --hold: an anonymous holder");
    let mut bytes = vec![0xff; 32 * PAGE as usize];
    assert_eq!(memory.read(&mut bytes, 0), Ok(bytes.len()));
    anonymous_contents(&bytes);
    println!("pagerprobe --hold: anonymous contents and zero holes read at a full page quota");
    // A peer that says a pipe end is a holder is answered as no holder, and
    // the process asking lives.
    let (read, _write) = toyos::pipe_pair().expect("pagerprobe --hold: a pipe");
    assert_eq!(
        Paged::received(toyos::OwnedHandle::from(read)).err(),
        Some(toyos::paged::NotAHolder::Other),
        "pagerprobe --hold: a pipe end was taken as a holder"
    );
    println!("pagerprobe --hold: a pipe end sent as a holder is refused, and this process lives");
    writes(&memory);
    let handle: toyos::OwnedHandle = Endowments::get().take(HOLDER).expect("pagerprobe --hold: endowed its holder");
    let holder = Paged::received(handle).expect("pagerprobe --hold: what it was endowed is a holder it may read");
    let mut page = [0u8; 16];
    match holder.read(&mut page, 0) {
        Err(SyscallError::Gone) => println!("pagerprobe --hold: the pager's end answered the wait Gone"),
        other => panic!("pagerprobe --hold: a wait on an unsupplied page answered {other:?}"),
    }
}

/// Through write holders: what a holder's write lands, and what it is refused.
/// `read_only` is a holder minted to read.
fn writes(read_only: &Paged) {
    let take = |name| {
        let handle: toyos::OwnedHandle = Endowments::get().take(name).expect("pagerprobe --hold: endowed a write holder");
        Paged::received(handle).expect("pagerprobe --hold: a write holder it may use")
    };
    let written = take(WRITTEN);
    let before = written.stat().expect("pagerprobe --hold: a statement before the writes");
    let page = vec![PAGE_BYTE; PAGE as usize];
    assert_eq!(written.write(&page, 0), Ok(page.len()), "pagerprobe --hold: a page written through a holder");
    let size = PAGE + TAIL.len() as u64;
    assert_eq!(written.append(&TAIL), Ok(size), "pagerprobe --hold: an append answers the size after it");
    let after = written.stat().expect("pagerprobe --hold: a statement after the writes");
    assert_eq!(after.size, size, "pagerprobe --hold: the size the writes left");
    assert!(after.mtime > before.mtime, "pagerprobe --hold: the writes moved the time from {} to {}", before.mtime, after.mtime);
    let mut back = vec![0; size as usize];
    assert_eq!(written.read(&mut back, 0), Ok(back.len()));
    assert!(back[..page.len()] == page[..] && back[page.len()..] == TAIL, "pagerprobe --hold: a holder reads back what it wrote");
    assert_eq!(written.write(&[1], PAGED_HOLDER_END), Err(SyscallError::WouldBlock), "pagerprobe --hold: a write past a holder's reach");
    let refused = written.stat().expect("pagerprobe --hold: a statement after the refusals");
    assert_eq!((refused.size, refused.mtime), (after.size, after.mtime), "pagerprobe --hold: a refused write changed the object");
    assert_eq!(read_only.write(&[1], 0), Err(SyscallError::PermissionDenied), "pagerprobe --hold: a holder minted to read wrote");
    println!("pagerprobe --hold: a holder's writes are admitted whole, and refused past its reach or rights");
    let credited = take(CREDITED);
    assert_eq!(credited.write(&page, 0), Ok(page.len()), "pagerprobe --hold: a page written against granted credit");
    println!("pagerprobe --hold: a page written against credit");
}

fn anonymous_contents(bytes: &[u8]) {
    assert!(bytes[..16 * PAGE as usize].iter().all(|&b| b == 0x37));
    assert!(bytes[16 * PAGE as usize..].iter().all(|&b| b == 0));
}
