//! A spawned child's working directory, as `SYS_SPAWN` takes it from its
//! caller: absolute, and no longer than the `MAX_PATH` a program reads it back
//! into by `SYS_GETCWD`. Nothing else is judged here: the directory is a file
//! server's, which the caller's client asked, and `.` and `..` are read off
//! the text by the client that uses it.

use toyos_abi::syscall::MAX_PATH;

/// Whether `cwd` may start a child.
pub fn spawnable(cwd: &str) -> bool {
    cwd.starts_with('/') && cwd.len() <= MAX_PATH
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    #[test]
    fn an_absolute_directory_that_fits_starts_a_child() {
        assert!(spawnable("/"));
        assert!(spawnable("/home/toy/../toy/./x"));
        let longest = alloc::format!("/{}", "a".repeat(MAX_PATH - 1));
        assert!(spawnable(&longest));
    }

    #[test]
    fn an_empty_relative_or_overlong_directory_is_refused() {
        assert!(!spawnable(""));
        assert!(!spawnable("home/toy"));
        assert!(!spawnable("./x"));
        let past: String = alloc::format!("/{}", "a".repeat(MAX_PATH));
        assert!(!spawnable(&past));
    }
}
