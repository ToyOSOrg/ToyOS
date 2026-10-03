//! What a test does to a guest over the cable: mint a key, run a program, move
//! a file, be refused.
//!
//! Every call here runs `tests/ssh-client-host`, which exits `0` when the
//! exchange happened — whatever the guest said — and `1` when it could not
//! complete one. Everything below turns the second into an error and only the
//! first into a verdict.

use std::path::Path;
use std::process::Command;

use super::compile;

/// A key pair minted for one test and thrown away with it, as two files in the
/// lane's scratch directory. **Nothing in this repository holds a private
/// key**: a committed one would be a credential with no owner and no expiry.
pub struct Identity {
    line: String,
}

impl Identity {
    /// The key kept in `dir`, minted the first time: a metal image carries its
    /// public half, so the key lives beside the image and outlives this run.
    pub fn mint_in(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let private = dir.join("id_ed25519");
        let public = dir.join("id_ed25519.pub");
        if !private.exists() || !public.exists() {
            let said = client(&["keygen", str(&private), str(&public)])?;
            if !said.trim().starts_with("ok ") {
                return Err(format!("the client answered {said:?} to keygen"));
            }
        }
        Ok(Identity {
            line: std::fs::read_to_string(&public)
                .map_err(|e| format!("read the minted public key: {e}"))?,
        })
    }

    /// The one `authorized_keys` line that names this key — what a test stages
    /// into the image so the guest will let it in.
    pub fn authorized_line(&self) -> String {
        self.line.clone()
    }
}

/// Run the client and hand back what it said, or the reason it could not say
/// anything. Its last line is the answer; the ones before it, if any, are a
/// listing's entries.
fn client(argv: &[&str]) -> Result<String, String> {
    let out = Command::new(toyos_build::build::ssh_client_host(&compile::repo_root()))
        .args(argv)
        .output()
        .map_err(|e| format!("run the ssh client: {e}"))?;
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        return Err(format!(
            "the ssh client could not complete `{}`: {}",
            argv.join(" "),
            said.trim()
        ));
    }
    Ok(said)
}

fn str(path: &Path) -> &str {
    path.to_str().expect("the lane's scratch paths are utf-8")
}

// --- The gate: one boot of `tests/sshdcase`, three judges on it ---

/// Where the image's `authorized_keys` file lands, ROOT-relative — the guest
/// reads it at `/system/etc/ssh_authorized_keys`, which `userland/sshd`'s
/// `AUTHORIZED_KEYS` is the other half of.
pub const KEYS_ON_ROOT: &str = "etc/ssh_authorized_keys";
