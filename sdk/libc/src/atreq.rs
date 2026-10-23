//! What `open`, the `*at` calls and the time setters decide before a path is
//! asked: the directory a path is resolved from, an open's flags, and the
//! times a caller asked for. It reads nothing but what it is handed, so the
//! host tests it (`toyos-libc-copies`).

use toyos::files::{At, Change, Open, Time};

use crate::errcode::{EBADF, EINVAL, ENOENT};

// The values `<fcntl.h>` declares.
pub(crate) const O_RDONLY: i32 = 0;
pub(crate) const O_WRONLY: i32 = 1;
pub(crate) const O_RDWR: i32 = 2;
pub(crate) const O_CREAT: i32 = 0x40;
pub(crate) const O_EXCL: i32 = 0x80;
pub(crate) const O_TRUNC: i32 = 0x200;
pub(crate) const O_APPEND: i32 = 0x400;
pub(crate) const O_DIRECTORY: i32 = 0x10000;
pub(crate) const O_CLOEXEC: i32 = 0x80000;
/// Linux's `open` flag, which POSIX does not define, with its value: it
/// includes [`O_DIRECTORY`], so a library that does not know it opens no file
/// where it asked for a directory.
pub(crate) const O_TMPFILE: i32 = 0x400000 | O_DIRECTORY;

pub(crate) const AT_FDCWD: i32 = -100;
pub(crate) const AT_SYMLINK_NOFOLLOW: i32 = 0x100;
pub(crate) const AT_REMOVEDIR: i32 = 0x200;
pub(crate) const AT_SYMLINK_FOLLOW: i32 = 0x400;
pub(crate) const AT_EMPTY_PATH: i32 = 0x1000;

/// `<sys/stat.h>`'s: a `tv_nsec` that sets the time to now, or leaves it.
pub(crate) const UTIME_NOW: i64 = (1 << 30) - 1;
pub(crate) const UTIME_OMIT: i64 = (1 << 30) - 2;

/// Where `path` is resolved from, `dirfd` as the `*at` calls take it: an
/// absolute path from neither, and a negative descriptor but [`AT_FDCWD`]
/// `EBADF` for any other. `toyos::files` refuses an empty path.
pub(crate) fn at(dirfd: i32, path: &[u8]) -> Result<At, i32> {
    match dirfd {
        _ if path.first() == Some(&b'/') => Ok(At::Cwd),
        AT_FDCWD => Ok(At::Cwd),
        fd if fd < 0 => Err(EBADF),
        fd => Ok(At::Fd(fd)),
    }
}

/// For `fstatat`, `linkat` and `utimensat`, whose `toyos::files` call reads
/// the empty path from a descriptor as the descriptor itself: that only with
/// `AT_EMPTY_PATH`, and `ENOENT` without it, as for every other call.
pub(crate) fn empty_path(path: &[u8], flags: i32) -> Result<(), i32> {
    match path.is_empty() && flags & AT_EMPTY_PATH == 0 {
        true => Err(ENOENT),
        false => Ok(()),
    }
}

/// `flags`, `EINVAL` when any bit is not in `allowed`.
pub(crate) fn only(flags: i32, allowed: i32) -> Result<i32, i32> {
    match flags & !allowed {
        0 => Ok(flags),
        _ => Err(EINVAL),
    }
}

/// Whether `open`'s `flags` ask to read and to write.
pub(crate) fn read_write(flags: i32) -> (bool, bool) {
    let access = flags & 3;
    (access == O_RDONLY || access == O_RDWR, access == O_WRONLY || access == O_RDWR)
}

/// What `open`'s `flags` ask `toyos::files` for. An unnamed file is opened to
/// write and is never also created at the name, as Linux's `open` defines
/// `O_TMPFILE`: `EINVAL`.
pub(crate) fn how(flags: i32) -> Result<Open, i32> {
    let (read, write) = read_write(flags);
    let unnamed = flags & O_TMPFILE == O_TMPFILE;
    if unnamed && (!write || flags & O_CREAT != 0) {
        return Err(EINVAL);
    }
    Ok(Open {
        read,
        write,
        append: flags & O_APPEND != 0,
        create: flags & O_CREAT != 0 && flags & O_EXCL == 0,
        create_new: flags & O_CREAT != 0 && flags & O_EXCL != 0,
        truncate: flags & O_TRUNC != 0,
        directory: flags & O_DIRECTORY != 0 && !unnamed,
        unnamed,
        cloexec: flags & O_CLOEXEC != 0,
    })
}

/// One time `utimensat` was handed, `(tv_sec, tv_nsec)`: `None` to leave it.
fn time(sec: i64, nsec: i64) -> Result<Option<Time>, i32> {
    match nsec {
        UTIME_NOW => Ok(Some(Time::Now)),
        UTIME_OMIT => Ok(None),
        0..=999_999_999 => {
            let secs = u64::try_from(sec).map_err(|_| EINVAL)?;
            let nanos = secs.checked_mul(1_000_000_000).and_then(|n| n.checked_add(nsec as u64)).ok_or(EINVAL)?;
            Ok(Some(Time::At(nanos)))
        }
        _ => Err(EINVAL),
    }
}

/// The change `utimensat`'s `times` asks for, access then modification, each
/// `(tv_sec, tv_nsec)`: both now where it was handed none. A time before the
/// epoch, which no file here keeps, is `EINVAL`.
pub(crate) fn times(times: Option<[(i64, i64); 2]>) -> Result<Change, i32> {
    let [(asec, ansec), (msec, mnsec)] = times.unwrap_or([(0, UTIME_NOW), (0, UTIME_NOW)]);
    Ok(Change { size: None, atime: time(asec, ansec)?, mtime: time(msec, mnsec)? })
}

/// [`times`] of `utimes`' `(tv_sec, tv_usec)` pair, which has no now and no
/// omission but its absence.
pub(crate) fn utimes(times: Option<[(i64, i64); 2]>) -> Result<Change, i32> {
    let Some(pair) = times else { return self::times(None) };
    let mut nanos = [(0, 0); 2];
    for (out, (sec, usec)) in nanos.iter_mut().zip(pair) {
        if !(0..1_000_000).contains(&usec) {
            return Err(EINVAL);
        }
        *out = (sec, usec * 1000);
    }
    self::times(Some(nanos))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_resolved_from_the_working_directory_a_descriptor_or_neither() {
        assert_eq!(at(AT_FDCWD, b"a"), Ok(At::Cwd));
        assert_eq!(at(3, b"a"), Ok(At::Fd(3)));
        assert_eq!(at(-1, b"a"), Err(EBADF));
        assert_eq!(at(-101, b"a"), Err(EBADF));
        // POSIX: an absolute path ignores the descriptor, a bad one included.
        assert_eq!(at(-1, b"/tmp/a"), Ok(At::Cwd));
        assert_eq!(at(7, b"/tmp/a"), Ok(At::Cwd));
    }

    /// Only `AT_EMPTY_PATH` lets the empty path name the descriptor itself.
    #[test]
    fn the_empty_path_is_the_descriptor_only_with_at_empty_path() {
        assert_eq!(empty_path(b"", 0), Err(ENOENT));
        assert_eq!(empty_path(b"", AT_SYMLINK_NOFOLLOW), Err(ENOENT));
        assert_eq!(empty_path(b"", AT_EMPTY_PATH), Ok(()));
        assert_eq!(empty_path(b"a", 0), Ok(()));
    }

    #[test]
    fn an_open_asks_for_what_its_flags_say() {
        let read = how(O_RDONLY).unwrap();
        assert!(read.read && !read.write && !read.directory && !read.unnamed);
        let dir = how(O_RDONLY | O_DIRECTORY | O_CLOEXEC).unwrap();
        assert!(dir.directory && dir.cloexec && !dir.unnamed);
        let new = how(O_WRONLY | O_CREAT | O_EXCL).unwrap();
        assert!(new.create_new && !new.create && new.write && !new.read);
        let tmp = how(O_TMPFILE | O_RDWR).unwrap();
        assert!(tmp.unnamed && tmp.read && tmp.write && !tmp.directory, "O_TMPFILE's directory bit names the directory it is made in");
    }

    /// `O_TMPFILE` without a write is `EINVAL`, and so is one that also
    /// creates; `O_DIRECTORY` alone is no unnamed file.
    #[test]
    fn an_unnamed_file_is_written_and_never_also_created() {
        assert_eq!(how(O_TMPFILE | O_RDONLY), Err(EINVAL));
        assert_eq!(how(O_TMPFILE | O_WRONLY | O_CREAT), Err(EINVAL));
        assert!(!how(O_DIRECTORY | O_RDWR).unwrap().unnamed);
    }

    #[test]
    fn a_flag_a_call_does_not_take_is_einval() {
        assert_eq!(only(AT_SYMLINK_NOFOLLOW, AT_SYMLINK_NOFOLLOW), Ok(AT_SYMLINK_NOFOLLOW));
        assert_eq!(only(0, AT_REMOVEDIR), Ok(0));
        let stat = AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH;
        assert_eq!(only(stat, stat), Ok(stat));
        assert_eq!(only(AT_REMOVEDIR | 1, AT_REMOVEDIR), Err(EINVAL));
        assert_eq!(only(AT_SYMLINK_FOLLOW, AT_SYMLINK_NOFOLLOW), Err(EINVAL));
    }

    #[test]
    fn utimensat_s_times_are_now_a_time_or_left() {
        let both_now = Change { size: None, atime: Some(Time::Now), mtime: Some(Time::Now) };
        assert_eq!(times(None), Ok(both_now));
        assert_eq!(times(Some([(0, UTIME_NOW), (0, UTIME_NOW)])), Ok(both_now));
        assert_eq!(
            times(Some([(5, UTIME_OMIT), (7, 9)])),
            Ok(Change { size: None, atime: None, mtime: Some(Time::At(7_000_000_009)) })
        );
        // The seconds beside UTIME_NOW and UTIME_OMIT are ignored, as POSIX has it.
        assert_eq!(times(Some([(-1, UTIME_OMIT), (-1, UTIME_NOW)])).unwrap().mtime, Some(Time::Now));
        for bad in [(0, -1), (0, 1_000_000_000), (-1, 0), (i64::MAX, 0)] {
            assert_eq!(times(Some([bad, (0, UTIME_OMIT)])), Err(EINVAL), "{bad:?}");
        }
    }

    #[test]
    fn utimes_takes_microseconds_and_nothing_past_a_second() {
        assert_eq!(utimes(None), times(None));
        assert_eq!(
            utimes(Some([(1, 2), (3, 999_999)])),
            Ok(Change { size: None, atime: Some(Time::At(1_000_002_000)), mtime: Some(Time::At(3_999_999_000)) })
        );
        assert_eq!(utimes(Some([(1, 1_000_000), (0, 0)])), Err(EINVAL));
        assert_eq!(utimes(Some([(1, -1), (0, 0)])), Err(EINVAL));
    }
}
