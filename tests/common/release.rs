//! The image release's command line for this host, booted as its notes print
//! it (`toyos_build::imagerelease::boots`) over a copy of the release image.

use toyos_build::build::{self, Boot};
use toyos_build::imagerelease::{self, Host};

/// The notes' line for this host boots the release image to a painting
/// desktop and leaves both firmware files as they were.
pub fn release_command_boots() -> Result<(), String> {
    let host = Host::this()?;
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
