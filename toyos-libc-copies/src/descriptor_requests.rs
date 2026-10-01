//! What `fcntl` decides before the kernel is asked, and `F_DUPFD`'s walk to
//! its floor over a handle table numbered as the kernel numbers one
//! (`kernel/src/object/handle.rs`).

use crate::fdreq::{self, Command};

/// What clang leaves in the register of an `int` argument of -1: `movl
/// $0xffffffff, %edx`, which zeroes the upper half (measured on
/// `206_libc_refusals.o`).
const MINUS_ONE_AS_CLANG_PASSES_IT: u64 = 0xffff_ffff;

#[test]
fn fcntl_reads_an_int_argument_from_the_lower_half_of_its_register() {
    for (cmd, arg, want) in [
        (0, MINUS_ONE_AS_CLANG_PASSES_IT, Command::Invalid),
        (0, u64::MAX, Command::Invalid),
        (0, 0xdead_beef_0000_000a, Command::DupAtLeast(10)),
        (0, 0, Command::DupAtLeast(0)),
        (0, 4095, Command::DupAtLeast(4095)),
        // `{OPEN_MAX}`, the slots a table has.
        (0, 4096, Command::Invalid),
        (0, 0x7fff_ffff, Command::Invalid),
        (1, u64::MAX, Command::GetFd),
        (2, 1, Command::SetFd(true)),
        (2, 0xffff_ffff_0000_0000, Command::SetFd(false)),
        (2, 0, Command::SetFd(false)),
        (3, 0, Command::Unsupported),
        (4, 0, Command::Unsupported),
        (5, 0, Command::Invalid),
        (6, 0, Command::Invalid),
        (7, 0, Command::Invalid),
        (8, 0, Command::Unsupported),
        (9, 0, Command::Unsupported),
        (1030, 10, Command::Unsupported),
        (12345, 0, Command::Invalid),
        (-1, 0, Command::Invalid),
    ] {
        assert_eq!(fdreq::command(cmd, arg), want, "fcntl(_, {cmd}, {arg:#x})");
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
            assert_eq!(most, 1, "floor {floor} held {most} duplicates at once (churned: {churned})");
            assert_eq!(table.borrow().holding(), before + 1, "floor {floor} left duplicates open (churned: {churned})");
        }
    }
}
