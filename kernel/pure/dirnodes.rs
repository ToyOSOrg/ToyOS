//! The numbers a kernel volume gives its directories
//! (`toyos_abi::syscall::Stat::node`).
//!
//! ROOT and `/tmp` keep no record of a directory: one is the prefix its names
//! share, or a name `mkdir` left in the VFS. So the VFS numbers each by its
//! absolute path, drawing the number on the first ask, and forgets it once a
//! removal beneath the path leaves no directory there: a number is never
//! another directory's while both exist. The paths are userland's choice, so
//! each is at most [`MAX_PATH`] bytes and charged to whoever caused it, the
//! charge `C` held beside its number until it is forgotten.

#![forbid(unsafe_code)]

use alloc::collections::BTreeMap;
use alloc::string::String;
use toyos_abi::syscall::{SyscallError, MAX_PATH};

pub struct Directories<C> {
    of: BTreeMap<String, (u64, C)>,
}

impl<C> Default for Directories<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C> Directories<C> {
    pub const fn new() -> Self {
        Self { of: BTreeMap::new() }
    }

    /// The number of the directory at `path`, drawn by `draw` when this does
    /// not hold one; `draw` answers a number nothing else on its volume holds.
    /// A path longer than [`MAX_PATH`] is refused `InvalidArgument`, as `cd`
    /// and `mkdir` refuse it, and a new one whose `charge` is refused with
    /// that refusal; either draws nothing and holds nothing.
    pub fn of(
        &mut self,
        path: &str,
        draw: impl FnOnce() -> u64,
        charge: impl FnOnce(&str) -> Result<C, SyscallError>,
    ) -> Result<u64, SyscallError> {
        if let Some(&(number, _)) = self.of.get(path) {
            return Ok(number);
        }
        if path.len() > MAX_PATH {
            return Err(SyscallError::InvalidArgument);
        }
        let charge = charge(path)?;
        let number = draw();
        self.of.insert(String::from(path), (number, charge));
        Ok(number)
    }

    /// A name at `path` was removed or moved away: `path` and every directory
    /// above it whose number this holds and that `exists` no longer answers for
    /// is forgotten, so a directory made there later is drawn a new one. The
    /// walk begins at the longest of them within [`MAX_PATH`], since nothing
    /// longer is held, so its length is bounded whatever `path`'s is.
    pub fn removed(&mut self, path: &str, mut exists: impl FnMut(&str) -> bool) {
        let mut at = path;
        if at.len() > MAX_PATH {
            match at.as_bytes()[..=MAX_PATH].iter().rposition(|&b| b == b'/') {
                Some(0) | None => return,
                Some(slash) => at = &at[..slash],
            }
        }
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
    use alloc::format;
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

    fn never() -> u64 {
        panic!("a held number is never drawn again, and a refused one never drawn")
    }

    /// A charge that is never refused.
    fn free(_: &str) -> Result<(), SyscallError> {
        Ok(())
    }

    /// Asked twice, a directory answers one number; two directories answer two.
    #[test]
    fn a_directory_is_one_number_and_two_are_two() {
        let mut draw = counter();
        let mut dirs = Directories::<()>::new();
        let a = dirs.of("/tmp/a", &mut draw, free).unwrap();
        let b = dirs.of("/tmp/b", &mut draw, free).unwrap();
        assert_ne!(a, b);
        assert_eq!(dirs.of("/tmp/a", never, free), Ok(a));
    }

    /// A removal that leaves the directory standing keeps its number; one that
    /// ends it forgets it, and the directory made there next is a new one.
    #[test]
    fn a_number_lasts_exactly_as_long_as_its_directory() {
        let mut draw = counter();
        let mut dirs = Directories::<()>::new();
        let a = dirs.of("/tmp/a", &mut draw, free).unwrap();

        dirs.removed("/tmp/a/f", |_| true);
        assert_eq!(dirs.of("/tmp/a", never, free), Ok(a));

        dirs.removed("/tmp/a/f", |dir| dir != "/tmp/a");
        let again = dirs.of("/tmp/a", &mut draw, free).unwrap();
        assert_ne!(again, a, "a directory made again where one was removed is another directory");
    }

    /// Every directory above the removal is judged, not only its parent, and
    /// one that only shares a prefix with it is not.
    #[test]
    fn a_removal_forgets_every_directory_it_ended_and_no_other() {
        let mut draw = counter();
        let mut dirs = Directories::<()>::new();
        let tmp = dirs.of("/tmp", &mut draw, free).unwrap();
        let a = dirs.of("/tmp/a", &mut draw, free).unwrap();
        let b = dirs.of("/tmp/a/b", &mut draw, free).unwrap();
        let sibling = dirs.of("/tmp/ab", &mut draw, free).unwrap();

        dirs.removed("/tmp/a/b/f", |dir| dir == "/tmp" || dir == "/tmp/ab");
        assert_eq!(dirs.of("/tmp", never, free), Ok(tmp));
        assert_eq!(dirs.of("/tmp/ab", never, free), Ok(sibling));
        let (a2, b2) = (dirs.of("/tmp/a", &mut draw, free).unwrap(), dirs.of("/tmp/a/b", &mut draw, free).unwrap());
        assert!(![tmp, a, b, sibling].contains(&a2) && ![tmp, a, b, sibling, a2].contains(&b2));
    }

    /// The mount's directories are asked about only where a number is held:
    /// a removal costs nothing for what nobody numbered.
    #[test]
    fn only_a_held_number_is_judged() {
        let mut dirs = Directories::<()>::new();
        dirs.of("/tmp/a", || 7, free).unwrap();
        let asked = Cell::new(Vec::new());
        dirs.removed("/tmp/a/b/c/f", |dir| {
            let mut seen = asked.take();
            seen.push(String::from(dir));
            asked.set(seen);
            true
        });
        assert_eq!(asked.take(), [String::from("/tmp/a")]);
    }

    /// A path of `MAX_PATH` bytes is numbered; one byte more is refused by
    /// name, and nothing is drawn or held for it.
    #[test]
    fn a_path_past_max_path_is_refused_invalid() {
        let mut draw = counter();
        let mut dirs = Directories::<()>::new();
        let longest = format!("/{}", "a".repeat(MAX_PATH - 1));
        assert!(dirs.of(&longest, &mut draw, free).is_ok());
        let past = format!("{longest}a");
        assert_eq!(dirs.of(&past, never, free), Err(SyscallError::InvalidArgument));
        assert_eq!(dirs.of.len(), 1);
    }

    /// A new directory whose charge is refused is refused with it, draws no
    /// number and holds nothing; one already numbered still answers.
    #[test]
    fn a_refused_charge_numbers_nothing() {
        let mut draw = counter();
        let mut dirs = Directories::<()>::new();
        let a = dirs.of("/tmp/a", &mut draw, free).unwrap();
        assert_eq!(dirs.of("/tmp/b", never, |_| Err(SyscallError::ResourceExhausted)), Err(SyscallError::ResourceExhausted));
        assert_eq!(dirs.of.len(), 1);
        assert_eq!(dirs.of("/tmp/a", never, |_| Err(SyscallError::ResourceExhausted)), Ok(a));
    }

    /// A removal beneath a path longer than `MAX_PATH` still judges every
    /// numbered directory above it.
    #[test]
    fn a_removal_past_max_path_judges_the_numbered_above_it() {
        let mut draw = counter();
        let mut dirs = Directories::<()>::new();
        let near = format!("/tmp/{}", "a".repeat(MAX_PATH - 5));
        assert_eq!(near.len(), MAX_PATH);
        let top = dirs.of("/tmp", &mut draw, free).unwrap();
        dirs.of(&near, &mut draw, free).unwrap();
        let deep = format!("{near}{}/f", "/b".repeat(MAX_PATH));
        dirs.removed(&deep, |dir| dir == "/tmp");
        assert_eq!(dirs.of("/tmp", never, free), Ok(top));
        assert_ne!(dirs.of(&near, &mut draw, free), Ok(2), "the directory MAX_PATH long ended and kept its number");
    }
}
