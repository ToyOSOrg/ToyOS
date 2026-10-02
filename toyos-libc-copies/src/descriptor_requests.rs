//! What `fcntl` decides before the kernel is asked, `F_DUPFD`'s walk to its
//! floor over a handle table numbered as the kernel numbers one
//! (`kernel/src/object/handle.rs`), and each descriptor's close-on-exec mark.

use crate::fdreq::{self, Command};
use crate::header::{self, FCNTL_H};

/// What clang leaves in the register of an `int` argument of -1: `movl
/// $0xffffffff, %edx`, which zeroes the upper half (measured on
/// `206_libc_refusals.o`).
const MINUS_ONE_AS_CLANG_PASSES_IT: u64 = 0xffff_ffff;

#[test]
fn fcntl_reads_an_int_argument_from_the_lower_half_of_its_register() {
    let set_cloexec = header::int(FCNTL_H, "FD_CLOEXEC") as u64;
    for (cmd, arg, want) in [
        ("F_DUPFD", MINUS_ONE_AS_CLANG_PASSES_IT, Command::Invalid),
        ("F_DUPFD", u64::MAX, Command::Invalid),
        ("F_DUPFD", 0xdead_beef_0000_000a, Command::DupAtLeast(10)),
        ("F_DUPFD", 0, Command::DupAtLeast(0)),
        ("F_DUPFD", 4095, Command::DupAtLeast(4095)),
        // `{OPEN_MAX}`, the slots a table has.
        ("F_DUPFD", 4096, Command::Invalid),
        ("F_DUPFD", 0x7fff_ffff, Command::Invalid),
        ("F_GETFD", u64::MAX, Command::GetFd),
        ("F_SETFD", set_cloexec, Command::SetFd(true)),
        ("F_SETFD", 0xffff_ffff_0000_0000, Command::SetFd(false)),
        ("F_SETFD", 0, Command::SetFd(false)),
        ("F_GETFL", 0, Command::Unsupported),
        ("F_SETFL", 0, Command::Unsupported),
        ("F_GETLK", 0, Command::Invalid),
        ("F_SETLK", 0, Command::Invalid),
        ("F_SETLKW", 0, Command::Invalid),
        ("F_SETOWN", 0, Command::Unsupported),
        ("F_GETOWN", 0, Command::Unsupported),
        ("F_DUPFD_CLOEXEC", 10, Command::Unsupported),
    ] {
        assert_eq!(fdreq::command(header::int(FCNTL_H, cmd), arg), want, "fcntl(_, {cmd}, {arg:#x})");
    }
    for cmd in [12345, -1] {
        assert_eq!(fdreq::command(cmd, 0), Command::Invalid, "fcntl(_, {cmd}, 0)");
    }
}

/// A handle table as the kernel keeps one: a fresh slot is the next index at
/// generation 0, numbered as the bare index; a closed slot goes on a free list
/// one generation on, and a `dup` takes the free list's last first.
struct Table {
    generations: Vec<u32>,
    held: Vec<bool>,
    free: Vec<usize>,
}

impl Table {
    fn with(open: usize) -> Table {
        Table { generations: vec![0; open], held: vec![true; open], free: Vec::new() }
    }

    fn dup(&mut self) -> Result<u32, ()> {
        let slot = match self.free.pop() {
            Some(slot) => slot,
            None if self.generations.len() < 4096 => {
                self.generations.push(0);
                self.held.push(false);
                self.generations.len() - 1
            }
            None => return Err(()),
        };
        self.held[slot] = true;
        Ok((self.generations[slot] << 12) | slot as u32)
    }

    fn close(&mut self, number: u32) {
        let slot = (number & 0xfff) as usize;
        assert!(self.held[slot] && self.generations[slot] == number >> 12, "closed {number:#x}, which is not held");
        self.held[slot] = false;
        self.generations[slot] += 1;
        self.free.push(slot);
    }

    fn holding(&self) -> usize {
        self.held.iter().filter(|&&h| h).count()
    }
}

/// Every floor under the slot count, over a table nothing has churned and over
/// one whose every slot has been closed once: the answer is at or above the
/// floor, and the walk never holds more than one duplicate besides it.
#[test]
fn f_dupfd_meets_its_floor_holding_one_duplicate_at_most() {
    for churned in [false, true] {
        for floor in [0u32, 1, 3, 4, 10, 255, 4094, 4095] {
            let mut table = Table::with(4);
            if churned {
                let all: Vec<u32> = (0..4092).map(|_| table.dup().unwrap()).collect();
                all.into_iter().for_each(|n| table.close(n));
            }
            let before = table.holding();
            let table = core::cell::RefCell::new(table);
            let mut most = 0;
            let answer = fdreq::dup_at_least(
                floor,
                || {
                    let mut t = table.borrow_mut();
                    most = most.max(t.holding() + 1 - before);
                    t.dup()
                },
                |below| table.borrow_mut().close(below),
            )
            .unwrap();
            assert!(answer >= floor, "floor {floor} answered {answer:#x} (churned: {churned})");
            if !churned && floor <= 4 {
                // The first duplicate, slot 4, meets the floor and is answered.
                assert_eq!(answer, 4, "floor {floor}");
            }
            assert_eq!(most, 1, "floor {floor} held {most} duplicates at once (churned: {churned})");
            assert_eq!(table.borrow().holding(), before + 1, "floor {floor} left duplicates open (churned: {churned})");
        }
    }
}

/// A descriptor's close-on-exec mark through `open` with and without
/// `O_CLOEXEC`, `F_SETFD` either way, and `F_GETFD`.
#[test]
fn each_descriptor_keeps_its_own_close_on_exec_mark() {
    let (cloexec, read_write) = (header::int(FCNTL_H, "O_CLOEXEC"), header::int(FCNTL_H, "O_RDWR"));
    let fd_cloexec = header::int(FCNTL_H, "FD_CLOEXEC");
    let mut marks = fdreq::CloseOnExec::new();
    marks.opened(3, read_write | cloexec);
    marks.opened(4, read_write);
    assert_eq!((marks.flags(3), marks.flags(4)), (fd_cloexec, 0), "open");

    marks.set(3, false);
    marks.set(4, true);
    assert_eq!((marks.flags(3), marks.flags(4)), (0, fd_cloexec), "F_SETFD");

    // A later handle in slot 4 (one generation on), which is a number of its own.
    assert_eq!(marks.flags(4 | 1 << 12), 0, "the next generation of slot 4");

    // `open` without `O_CLOEXEC` of a number marked.
    marks.set(5, true);
    marks.opened(5, read_write);
    assert_eq!(marks.flags(5), 0, "open over a mark");
}
