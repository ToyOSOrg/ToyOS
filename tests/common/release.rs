//! The image release's macOS command line, booted as its notes print it
//! (`toyos_build::imagerelease::boots`) over a copy of the release image.

use toyos_build::build::{self, Boot};
use toyos_build::imagerelease::{self, Host};

/// The name `src/redlist.rs` knows this test by.
pub const NAME: &str = "release_command_boots";

/// The notes' Apple Silicon line boots the release image to a painting
/// desktop and leaves both firmware files as they were. Run by the suite's
/// `--release-command` and by nothing else.
pub fn release_command_boots() -> Result<(), String> {
    let host = Host::MacosAppleSilicon;
    let root = super::compile::repo_root();
    let boot = Boot::release(&root);
    let plan = build::plan_for(&root, &boot, false, &[]);
    let image = build::build(&root, boot, false, &plan);
    let dir = super::lane::dir();
    let stick = dir.join("release-command.img");
    std::fs::copy(&image, &stick).map_err(|e| format!("copy the release image to {}: {e}", stick.display()))?;
    let ceiling = super::qemu::budget(imagerelease::DESKTOP);
    imagerelease::boots(&root, host, &stick, ceiling, &dir.join("release-command.stderr"))?;
    eprintln!("  [release] {host:?}'s command line reached a painting desktop");
    Ok(())
}
