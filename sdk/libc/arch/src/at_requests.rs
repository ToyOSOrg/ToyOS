//! The `*at` calls' and `open`'s words as `include/fcntl.h` and
//! `include/sys/stat.h` declare them, against the ones `atreq` decides by.

use crate::atreq;
use crate::header::{self, FCNTL_H, STAT_H};

#[test]
fn every_word_the_at_calls_decide_by_is_the_headers() {
    let fcntl = [
        ("O_RDONLY", atreq::O_RDONLY),
        ("O_WRONLY", atreq::O_WRONLY),
        ("O_RDWR", atreq::O_RDWR),
        ("O_CREAT", atreq::O_CREAT),
        ("O_EXCL", atreq::O_EXCL),
        ("O_TRUNC", atreq::O_TRUNC),
        ("O_APPEND", atreq::O_APPEND),
        ("O_DIRECTORY", atreq::O_DIRECTORY),
        ("O_CLOEXEC", atreq::O_CLOEXEC),
        ("O_TMPFILE", atreq::O_TMPFILE),
        ("AT_FDCWD", atreq::AT_FDCWD),
        ("AT_SYMLINK_NOFOLLOW", atreq::AT_SYMLINK_NOFOLLOW),
        ("AT_REMOVEDIR", atreq::AT_REMOVEDIR),
        ("AT_SYMLINK_FOLLOW", atreq::AT_SYMLINK_FOLLOW),
        ("AT_EMPTY_PATH", atreq::AT_EMPTY_PATH),
    ];
    for (name, value) in fcntl {
        assert_eq!(header::int(FCNTL_H, name), value, "{name}");
    }
    assert_eq!(i64::from(header::int(STAT_H, "UTIME_NOW")), atreq::UTIME_NOW);
    assert_eq!(i64::from(header::int(STAT_H, "UTIME_OMIT")), atreq::UTIME_OMIT);
}
