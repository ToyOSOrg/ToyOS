//! A pager whose end is what a test watches, started by its row's `pager`.
//!
//! As the pager: it makes an object it never supplies and a holder of it,
//! is refused a read through its own holder (no pager waits on a pager), and
//! starts itself again as a helper, `--hold`, endowed the holder. Once its
//! notices say the helper waits on the object's first page, it lets its
//! pager go, and exits as the helper did.
//!
//! As the helper: a holder's read of a page nobody supplies, which only the
//! pager's end answers, `Gone`; it exits 0 on exactly that.

use std::os::toyos::process::CommandExt;
use std::process::Command;
use std::time::{Duration, Instant};

use toyos::endow::Endowments;
use toyos::paged::{Notice, PagedNotice, Paged, Pager};
use toyos::poller::{Poller, READABLE};
use toyos_abi::syscall::SyscallError;

const HOLDER: &str = "pagerprobe-holder";
const SELF: &str = "/system/bin/pagerprobe";

/// How long the helper may take to park before the probe gives up on it: a
/// failure bound, never what ends a passing run.
const PARK_BOUND: Duration = Duration::from_secs(30);

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--hold") {
        return hold();
    }
    let pager = Pager::create(16, 0, 4, Duration::from_secs(1)).expect("pagerprobe: its row starts it as a pager");
    let object = pager.object(4096).expect("pagerprobe: an object");
    let own = pager.holder(object).expect("pagerprobe: a holder for itself");
    // The handle is a holder, and this process a pager: refused by the kernel.
    assert_eq!(
        Paged::received(own).err(),
        Some(toyos::paged::NotAHolder::Pager),
        "pagerprobe: a pager was let read a holder, so it could wait on a pager"
    );
    println!("pagerprobe: a pager is refused a holder's read");

    let theirs = pager.holder(object).expect("pagerprobe: a holder to hand on");
    let mut helper = Command::new(SELF).arg("--hold").endow(HOLDER, theirs.into()).spawn().expect("pagerprobe: the helper starts");

    // The helper's request is the notice that it waits.
    let poller = Poller::new(1);
    let began = Instant::now();
    let mut buf = [PagedNotice::default(); 4];
    loop {
        if pager.notices(&mut buf).expect("pagerprobe: its notices").any(|n| matches!(n, Notice::Requested { object: o, first: 0, .. } if o == object)) {
            break;
        }
        let left = PARK_BOUND.checked_sub(began.elapsed()).expect("pagerprobe: the helper never waited on the page");
        poller.watch(&pager, READABLE, 0);
        poller.wait(1, left.as_nanos() as u64, |_| {});
    }
    println!("pagerprobe: the helper waits on a page nobody supplies; ending the pager");
    drop(pager);
    let status = helper.wait().expect("pagerprobe: the helper's end");
    assert!(status.success(), "pagerprobe: the helper ended {status:?}");
    println!("pagerprobe: ok");
}

fn hold() {
    let handle: toyos::OwnedHandle = Endowments::get().take(HOLDER).expect("pagerprobe --hold: endowed its holder");
    let holder = Paged::received(handle).expect("pagerprobe --hold: what it was endowed is a holder it may read");
    let mut page = [0u8; 16];
    match holder.read(&mut page, 0) {
        Err(SyscallError::Gone) => println!("pagerprobe --hold: the pager's end answered the wait Gone"),
        other => panic!("pagerprobe --hold: a wait on an unsupplied page answered {other:?}"),
    }
}
