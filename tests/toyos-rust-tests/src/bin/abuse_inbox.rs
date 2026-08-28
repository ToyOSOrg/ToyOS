//! The kernel must survive a process that lies in its own submission ring header.
//!
//! `head`/`tail` live in the 2 MiB page the process maps and writes itself, so
//! `tail - head` is a userland-chosen number that must never size a kernel
//! allocation or index past the ring depth.
//!
//! It also holds the one assertion in the tree on an `OP_WATCH` result word: a
//! two-direction watch must report the readiness that occurred, not the interest.

use core::sync::atomic::Ordering;

use toyos::AsHandle;
use toyos_abi::inbox::{
    Completion, RingHeader, Submission, COMPLETION_RING_OFF, OP_NOP, OP_WATCH, READABLE,
    SUBMISSIONS_OFF, SUBMISSION_RING_OFF, WRITABLE,
};
use toyos_abi::syscall::{self, SyscallError};

const DEPTH: u32 = 8;

/// A `READABLE | WRITABLE` watch on a pipe read end that has a byte queued must
/// complete with `READABLE` alone. The kernel refuses `WRITABLE` asked alone on
/// that handle (a read end has no write source at all), so the word it hands
/// back for the pair must not affirm a writability it denies.
fn watch_reports_only_the_readiness_that_fired() {
    let (read, write) = toyos::pipe_pair().expect("a pipe of our own");
    write.write_nonblock(b"x").expect("one byte into the write end");

    let (inbox, base) = unsafe { syscall::inbox_setup(DEPTH) }.expect("inbox_setup");
    let ring = unsafe { &*(base.add(SUBMISSION_RING_OFF as usize) as *const RingHeader) };
    let head = ring.head.load(Ordering::Acquire);
    let idx = (head & (DEPTH - 1)) as usize;
    let submission = unsafe {
        &mut *(base.add(SUBMISSIONS_OFF as usize + idx * core::mem::size_of::<Submission>())
            as *mut Submission)
    };
    *submission = Submission::default();
    submission.op = OP_WATCH;
    submission.handle = read.as_handle();
    submission.op_flags = READABLE | WRITABLE;
    submission.token = 0x5EED;
    ring.tail.store(head.wrapping_add(1), Ordering::Release);

    let completed = syscall::inbox_submit(inbox, 1, 1, 0).expect("watch submission");
    assert_eq!(completed, 1, "the readable watch did not complete");

    let cq = unsafe { &*(base.add(COMPLETION_RING_OFF as usize) as *const RingHeader) };
    let ch = cq.head.load(Ordering::Acquire);
    assert_ne!(ch, cq.tail.load(Ordering::Acquire), "no completion was posted");
    let cidx = (ch & (cq.ring_size - 1)) as usize;
    let completion = unsafe {
        &*(base.add(COMPLETION_RING_OFF as usize
            + core::mem::size_of::<RingHeader>()
            + cidx * core::mem::size_of::<Completion>()) as *const Completion)
    };
    assert_eq!(completion.token, 0x5EED, "the completion is for another submission");
    assert_eq!(
        completion.result as u32,
        READABLE,
        "a READABLE|WRITABLE watch on a pipe read end reported {}, not READABLE alone — \
         the kernel echoed the interest mask instead of the readiness that fired",
        completion.result,
    );

    syscall::close(inbox);
    println!("watch result: a two-direction watch reports the direction that fired");
}

fn main() {
    let (inbox, base) = unsafe { syscall::inbox_setup(DEPTH) }.expect("inbox_setup");
    let ring = unsafe { &*(base.add(SUBMISSION_RING_OFF as usize) as *const RingHeader) };

    // 4 million entries claimed in an 8-entry ring: 160 MB of Submission.
    ring.tail.store(4_000_000, Ordering::Release);
    let err = syscall::inbox_submit(inbox, 4_000_000, 0, 0)
        .expect_err("enter must reject a submission tail beyond the ring depth");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for bogus tail");

    // Saturated on both sides: the capacity computation itself overflows.
    ring.tail.store(u32::MAX, Ordering::Release);
    let err = syscall::inbox_submit(inbox, u32::MAX, 0, 0)
        .expect_err("enter must reject a saturated submission tail");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for saturated tail");

    // An honest ring with a to_submit larger than it could ever hold.
    ring.tail.store(ring.head.load(Ordering::Acquire), Ordering::Release);
    let err = syscall::inbox_submit(inbox, 1_000_000, 0, 0)
        .expect_err("enter must reject to_submit beyond the ring depth");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for bogus to_submit");

    // The ring still works: the rejections must not have advanced head or
    // left the instance in a state where honest submissions stop completing.
    let head = ring.head.load(Ordering::Acquire);
    ring.tail.store(head, Ordering::Release);
    let idx = head & (DEPTH - 1);
    let submission = unsafe {
        &mut *(base.add(SUBMISSIONS_OFF as usize + idx as usize * core::mem::size_of::<Submission>())
            as *mut Submission)
    };
    *submission = Submission::default();
    submission.op = OP_NOP;
    submission.token = 0xC0FFEE;
    ring.tail.store(head.wrapping_add(1), Ordering::Release);

    let completions = syscall::inbox_submit(inbox, 1, 1, 0).expect("honest NOP submission");
    assert_eq!(completions, 1, "NOP did not complete after the rejected batches");

    syscall::close(inbox);
    println!("submission ring header abuse rejected, inbox still usable");

    watch_reports_only_the_readiness_that_fired();
}
