//! A Unix-domain socket path short enough for any `$TMPDIR` a host might hand
//! out.
//!
//! Darwin's `sockaddr_un.sun_path` holds 104 bytes, NUL included — Linux's
//! holds 108 — and a macOS `$TMPDIR`
//! (`/private/var/folders/<2>/<27ish random>/T`) plus this project's own lane
//! path (`toyos-tmp-<pid>-<n>/tests-<n>/lane-<n>/`) can already spend every one
//! of those 104 bytes before a filename is added: seen on `lan_mdns_answer`,
//! `connect to QEMU's /private/var/folders/.../T/toyos-tmp-55923-0/tests-0/lane-2/tap-out-0.sock:
//! path must be shorter than SUN_LEN`. [`short`] sits under `/tmp` directly —
//! not `$TMPDIR`, whose canonicalized macOS form is what grew that deep — so a
//! caller never inherits the host's own `$TMPDIR` depth.

use std::path::PathBuf;

/// Darwin's `sockaddr_un.sun_path`, the tighter of the two platforms this
/// project runs guests on; a path under this fits Linux's 108-byte one too.
const DARWIN_SUN_PATH: usize = 104;

/// A path for a Unix-domain socket named `label`, this process's `n`th one.
/// Short on every host: fixed at `/tmp`, never `$TMPDIR`.
pub fn short(label: &str, n: u32) -> PathBuf {
    let path = PathBuf::from(format!("/tmp/toyos-{label}-{}-{n}.sock", std::process::id()));
    assert!(
        path.as_os_str().len() < DARWIN_SUN_PATH,
        "{path:?} does not fit a {DARWIN_SUN_PATH}-byte sockaddr_un.sun_path"
    );
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape the failure was seen under: a macOS `$TMPDIR` plus this
    /// project's lane path already fills every byte `sockaddr_un.sun_path`
    /// gives it, with nothing left for a filename — the old construction this
    /// module replaces.
    #[test]
    fn the_lane_path_a_typical_macos_tmpdir_produced_did_not_fit() {
        let tmpdir = "/private/var/folders/gr/mr4_fg4n34jb417sx1g5cgxc0000gp/T";
        let old = format!("{tmpdir}/toyos-tmp-55923-0/tests-0/lane-2/tap-out-0.sock");
        assert!(
            old.len() >= DARWIN_SUN_PATH,
            "{old:?} ({} bytes) was expected to no longer fit, and does",
            old.len()
        );
    }

    /// [`short`] never depends on `$TMPDIR`, so a six-digit pid and a sequence
    /// number both past anything this harness has produced yet still fit.
    #[test]
    fn a_short_path_fits_regardless_of_the_hosts_tmpdir() {
        let path = short("tap-out", 999_999);
        assert!(path.as_os_str().len() < DARWIN_SUN_PATH, "{path:?}");
        assert!(path.starts_with("/tmp"), "{path:?}");
    }
}
