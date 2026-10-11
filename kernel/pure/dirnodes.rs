//! The numbers a kernel volume gives its directories
//! (`toyos_abi::syscall::Stat::node`).
//!
//! ROOT and `/tmp` keep no record of a directory: one is the prefix its names
//! share, or a name `mkdir` left in the VFS. So the VFS numbers each by its
//! absolute path, drawing the number on the first ask, and forgets it once a
//! removal beneath the path leaves no directory there: a number is never
//! another directory's while both exist, and what this holds is bounded by the
//! directories that exist and were asked about.

#![forbid(unsafe_code)]

use alloc::collections::BTreeMap;
use alloc::string::String;

#[derive(Default)]
pub struct Directories {
    of: BTreeMap<String, u64>,
}

impl Directories {
    pub const fn new() -> Self {
        Self { of: BTreeMap::new() }
    }

    /// The number of the directory at `path`, drawn by `draw` when this does
    /// not hold one; `draw` answers a number nothing else on its volume holds.
    pub fn of(&mut self, path: &str, draw: impl FnOnce() -> u64) -> u64 {
        if let Some(&number) = self.of.get(path) {
            return number;
        }
        let number = draw();
        self.of.insert(String::from(path), number);
        number
    }

    /// A name at `path` was removed or moved away: `path` and every directory
    /// above it whose number this holds and that `exists` no longer answers for
    /// is forgotten, so a directory made there later is drawn a new one.
    pub fn removed(&mut self, path: &str, mut exists: impl FnMut(&str) -> bool) {
        let mut at = path;
        loop {
            if self.of.contains_key(at) && !exists(at) {
                self.of.remove(at);
            }
            match at.rfind('/') {
                Some(0) | None => return,
                Some(slash) => at = &at[..slash],
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::vec::Vec;

    /// A counter that never rewinds, as the file cache's is.
    fn counter() -> impl FnMut() -> u64 {
        let mut next = 0;
        move || {
            next += 1;
            next
        }
    }

    /// Asked twice, a directory answers one number; two directories answer two.
    #[test]
    fn a_directory_is_one_number_and_two_are_two() {
        let mut draw = counter();
        let mut dirs = Directories::new();
        let a = dirs.of("/tmp/a", &mut draw);
        let b = dirs.of("/tmp/b", &mut draw);
        assert_ne!(a, b);
        assert_eq!(dirs.of("/tmp/a", || panic!("a held number is never drawn again")), a);
    }

    /// A removal that leaves the directory standing keeps its number; one that
    /// ends it forgets it, and the directory made there next is a new one.
    #[test]
    fn a_number_lasts_exactly_as_long_as_its_directory() {
        let mut draw = counter();
        let mut dirs = Directories::new();
        let a = dirs.of("/tmp/a", &mut draw);

        dirs.removed("/tmp/a/f", |_| true);
        assert_eq!(dirs.of("/tmp/a", || panic!("a directory that stands kept its number")), a);

        dirs.removed("/tmp/a/f", |dir| dir != "/tmp/a");
        let again = dirs.of("/tmp/a", &mut draw);
        assert_ne!(again, a, "a directory made again where one was removed is another directory");
    }

    /// Every directory above the removal is judged, not only its parent, and
    /// one that only shares a prefix with it is not.
    #[test]
    fn a_removal_forgets_every_directory_it_ended_and_no_other() {
        let mut draw = counter();
        let mut dirs = Directories::new();
        let tmp = dirs.of("/tmp", &mut draw);
        let a = dirs.of("/tmp/a", &mut draw);
        let b = dirs.of("/tmp/a/b", &mut draw);
        let sibling = dirs.of("/tmp/ab", &mut draw);

        dirs.removed("/tmp/a/b/f", |dir| dir == "/tmp" || dir == "/tmp/ab");
        assert_eq!(dirs.of("/tmp", || panic!("/tmp stands")), tmp);
        assert_eq!(dirs.of("/tmp/ab", || panic!("a name sharing the prefix is not above the removal")), sibling);
        let (a2, b2) = (dirs.of("/tmp/a", &mut draw), dirs.of("/tmp/a/b", &mut draw));
        assert!(![tmp, a, b, sibling].contains(&a2) && ![tmp, a, b, sibling, a2].contains(&b2));
    }

    /// The mount's directories are asked about only where a number is held:
    /// a removal costs nothing for what nobody numbered.
    #[test]
    fn only_a_held_number_is_judged() {
        let mut dirs = Directories::new();
        dirs.of("/tmp/a", || 7);
        let asked = Cell::new(Vec::new());
        dirs.removed("/tmp/a/b/c/f", |dir| {
            let mut seen = asked.take();
            seen.push(String::from(dir));
            asked.set(seen);
            true
        });
        assert_eq!(asked.take(), [String::from("/tmp/a")]);
    }
}
