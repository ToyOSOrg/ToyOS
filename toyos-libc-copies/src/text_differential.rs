//! `strnlen` against the host C library's, over every bound around every
//! string in a buffer that ends in no NUL; and `strsignal`'s texts: one of its
//! own for each signal `include/signal.h` numbers, in glibc's words.

use std::collections::BTreeSet;

use crate::header;
use crate::text;

extern "C" {
    #[link_name = "strnlen"]
    fn host_strnlen(s: *const u8, max: usize) -> usize;
}

#[test]
fn strnlen_is_the_host_libraries() {
    let buf = b"abc\0\0defgh\0x";
    for start in 0..buf.len() {
        for max in 0..=buf.len() - start {
            let s = buf[start..].as_ptr();
            // SAFETY: `max` bytes from `s` are inside `buf`, and neither reads
            // past `s + max` nor past a NUL.
            let (ours, host) = unsafe { (text::strnlen(s, max), host_strnlen(s, max)) };
            assert_eq!(ours, host, "strnlen at {start}, bounded by {max}");
        }
    }
}

/// glibc's `strsignal` for each number Linux gives a signal `signal.h` names.
const GLIBC: &[(&str, &str)] = &[
    ("SIGHUP", "Hangup"),
    ("SIGINT", "Interrupt"),
    ("SIGQUIT", "Quit"),
    ("SIGILL", "Illegal instruction"),
    ("SIGTRAP", "Trace/breakpoint trap"),
    ("SIGABRT", "Aborted"),
    ("SIGBUS", "Bus error"),
    ("SIGFPE", "Floating point exception"),
    ("SIGKILL", "Killed"),
    ("SIGUSR1", "User defined signal 1"),
    ("SIGSEGV", "Segmentation fault"),
    ("SIGUSR2", "User defined signal 2"),
    ("SIGPIPE", "Broken pipe"),
    ("SIGALRM", "Alarm clock"),
    ("SIGTERM", "Terminated"),
    ("SIGCHLD", "Child exited"),
    ("SIGCONT", "Continued"),
    ("SIGSTOP", "Stopped (signal)"),
];

fn text_of(sig: i32) -> String {
    let bytes = text::signal_text(sig);
    let (last, text) = bytes.split_last().expect("an empty text");
    assert_eq!(*last, 0, "strsignal({sig}) is not NUL-terminated");
    assert!(!text.contains(&0), "strsignal({sig}) has a NUL inside");
    String::from_utf8(text.to_vec()).unwrap()
}

#[test]
fn every_signal_signal_h_numbers_has_glibcs_text() {
    let numbered = header::signals();
    assert_eq!(numbered.len(), GLIBC.len(), "signal.h numbers {numbered:?}");
    let mut texts = BTreeSet::new();
    for (name, sig) in numbered {
        let (_, words) = GLIBC.iter().find(|(n, _)| *n == name).unwrap_or_else(|| panic!("{name}: no glibc text to hold strsignal to"));
        assert_eq!(text_of(sig), *words, "strsignal({name} = {sig})");
        texts.insert(text_of(sig));
    }
    assert_eq!(texts.len(), GLIBC.len(), "two signals share a text");
    for unnamed in [0, 16, 20, 64, -1, i32::MAX] {
        assert_eq!(text_of(unnamed), "Unknown signal", "strsignal({unnamed})");
    }
}
