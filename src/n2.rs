//! n2, the Rust Ninja, and the only Ninja the build runs: under the name
//! `ninja`, which is how CMake's Ninja generator and rustc's bootstrap ask for
//! one.

use std::path::{Path, PathBuf};
use std::process::Command;

/// cargo's install of n2 but for its `--root`: without its default jemalloc,
/// which is C.
pub(crate) const N2: [&str; 7] = [
    "install",
    "--locked",
    "--no-default-features",
    "--git",
    "https://github.com/evmar/n2",
    "--rev",
    "b1fead52ccda0c497d816696f23f4099c3e8ec1f",
];

/// Where [`N2`] installs under `root`: a directory every argument names, so a
/// moved pin or flag names an empty one.
fn installed(root: &Path) -> PathBuf {
    root.join("target/n2").join(crate::sysroot::short(N2.join("\0").as_bytes()))
}

/// The directory [`N2`] installs n2 in under `root`, installing it unless it is
/// there. It holds n2 and nothing else but a link to it named `ninja`, the name
/// under which n2 speaks the Ninja CMake asks for.
pub fn bin(root: &Path) -> PathBuf {
    let dir = installed(root);
    let bin = dir.join("bin");
    let ninja = bin.join("ninja");
    // The link is made only once cargo has installed what it names.
    if !ninja.is_file() {
        let status = Command::new("cargo")
            .args(N2)
            .arg("--root")
            .arg(&dir)
            .env_remove("RUSTFLAGS")
            .status()
            .unwrap_or_else(|e| panic!("cargo failed to launch: {e}"));
        assert!(status.success(), "n2 did not install into {}", dir.display());
        std::os::unix::fs::symlink("n2", &ninja).unwrap_or_else(|e| panic!("symlink {} -> n2: {e}", ninja.display()));
        assert!(ninja.is_file(), "n2 installed into {}, and {} names no file", dir.display(), ninja.display());
    }
    bin
}

/// n2 by the name `ninja`, in [`bin`].
pub fn ninja(root: &Path) -> PathBuf {
    bin(root).join("ninja")
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;

    use super::*;
    use toyos_tmpdir::TempDir;

    /// What [`bin`] leaves under `root`, with an empty `n2`.
    pub(crate) fn installed_stand_in(root: &Path) -> PathBuf {
        let bin = installed(root).join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("n2"), b"").unwrap();
        std::os::unix::fs::symlink("n2", bin.join("ninja")).unwrap();
        bin
    }

    /// **n2 is installed once**: with the link and n2 in the directory the pin
    /// names, no cargo runs, and the n2 there is the one returned.
    #[test]
    fn an_installed_n2_is_not_installed_again() {
        let root = TempDir::new("n2");
        let stand_in = installed_stand_in(&root);

        assert_eq!(bin(&root), stand_in);
        assert_eq!(ninja(&root), stand_in.join("ninja"));
        assert!(fs::read(stand_in.join("n2")).unwrap().is_empty(), "the installed n2 was replaced");
    }
}
