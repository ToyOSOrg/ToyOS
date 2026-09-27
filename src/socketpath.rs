//! A Unix-domain socket's name that fits `sockaddr_un.sun_path` on every host,
//! and is gone when its holder is.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// A socket name under `/tmp`, removed when this is dropped: on a return and on
/// a panic's unwind alike.
#[derive(Debug)]
pub struct Socket(PathBuf);

impl Socket {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        match std::fs::remove_file(&self.0) {
            Ok(()) => {}
            // Whoever was to bind it never ran, or unlinked it on its way out.
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            // A second panic here would abort and lose the first one's message.
            Err(e) if std::thread::panicking() => eprintln!("remove {}: {e}", self.0.display()),
            Err(e) => panic!("remove {}: {e}", self.0.display()),
        }
    }
}

/// This process's `n`th socket named `label`. Under `/tmp` and never `$TMPDIR`,
/// whose depth is the host's to choose.
pub fn short(label: &str, n: u32) -> Socket {
    let path = PathBuf::from(format!("/tmp/toyos-{label}-{}-{n}.sock", std::process::id()));
    // A run killed before its drop left this name, under a pid now reused.
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => panic!("remove the stale {}: {e}", path.display()),
    }
    Socket(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Darwin's `sockaddr_un.sun_path`, NUL included; Linux's is 108.
    const DARWIN_SUN_PATH: usize = 104;

    #[test]
    fn a_short_path_fits_regardless_of_the_hosts_tmpdir() {
        let socket = short("tap-out", u32::MAX);
        let path = socket.path();
        assert!(path.as_os_str().len() < DARWIN_SUN_PATH, "{path:?}");
        assert!(path.starts_with("/tmp"), "{path:?}");
    }

    #[test]
    fn a_dropped_socket_takes_its_name_with_it() {
        let socket = short("drop", u32::MAX);
        let path = socket.path().to_path_buf();
        std::fs::write(&path, b"").expect("stand in for the bound socket");
        drop(socket);
        assert!(!path.exists(), "{path:?} outlived its holder");
    }
}
