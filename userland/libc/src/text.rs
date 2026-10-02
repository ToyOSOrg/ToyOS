//! C strings and texts: `strnlen`'s scan and `strsignal`'s descriptions. It
//! reads and sets nothing but what it is handed, so the host tests it
//! (`toyos-libc-copies`).

/// The length of the string at `s`, never reading `s + max` or past it.
///
/// # Safety
/// `s` is readable up to its first NUL or for `max` bytes, whichever is first.
pub(crate) unsafe fn strnlen(s: *const u8, max: usize) -> usize {
    let mut n = 0;
    while n < max && unsafe { *s.add(n) } != 0 {
        n += 1;
    }
    n
}

/// `strsignal`'s text for `sig`, NUL-terminated: glibc's words for each
/// signal `include/signal.h` numbers, and one text for every other number,
/// which POSIX leaves to the implementation.
pub(crate) fn signal_text(sig: i32) -> &'static [u8] {
    match sig {
        1 => b"Hangup\0",
        2 => b"Interrupt\0",
        3 => b"Quit\0",
        4 => b"Illegal instruction\0",
        5 => b"Trace/breakpoint trap\0",
        6 => b"Aborted\0",
        7 => b"Bus error\0",
        8 => b"Floating point exception\0",
        9 => b"Killed\0",
        10 => b"User defined signal 1\0",
        11 => b"Segmentation fault\0",
        12 => b"User defined signal 2\0",
        13 => b"Broken pipe\0",
        14 => b"Alarm clock\0",
        15 => b"Terminated\0",
        17 => b"Child exited\0",
        18 => b"Continued\0",
        19 => b"Stopped (signal)\0",
        _ => b"Unknown signal\0",
    }
}
