//! What a test does to a guest over the cable: mint a key, run a program, move
//! a file, be refused.
//!
//! Every call here runs `tests/ssh-client-host`, which exits `0` when the
//! exchange happened — whatever the guest said — and `1` when it could not
//! complete one. Everything below turns the second into an error and only the
//! first into a verdict.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::compile;
use super::qemu::SSH_FORWARD_HOST;

/// Where the guest is, as this host sees it through QEMU's forward.
pub const HOST: &str = SSH_FORWARD_HOST;

/// A key pair minted for one test and thrown away with it, as two files in the
/// lane's scratch directory. **Nothing in this repository holds a private
/// key**: a committed one would be a credential with no owner and no expiry.
pub struct Identity {
    private: PathBuf,
    line: String,
    fingerprint: String,
}

impl Identity {
    /// The key called `name` in this lane, minted the first time it is asked
    /// for and handed back after that: a boot several tests share stages one
    /// of these into its image, so a second mint would hand the later members
    /// a key the running guest has never heard of.
    pub fn mint(name: &str) -> Result<Self, String> {
        let dir = super::lane::dir().join("ssh").join(name);
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let private = dir.join("id_ed25519");
        let public = dir.join("id_ed25519.pub");
        let fingerprint = dir.join("fingerprint");
        if !private.exists() || !public.exists() || !fingerprint.exists() {
            let said = client(&["keygen", str(&private), str(&public)])?;
            let said = said
                .trim()
                .strip_prefix("ok ")
                .ok_or_else(|| format!("the client answered {said:?} to keygen"))?;
            std::fs::write(&fingerprint, said)
                .map_err(|e| format!("record the minted key's fingerprint: {e}"))?;
        }
        Ok(Identity {
            line: std::fs::read_to_string(&public)
                .map_err(|e| format!("read the minted public key: {e}"))?,
            fingerprint: std::fs::read_to_string(&fingerprint)
                .map_err(|e| format!("read the minted key's fingerprint: {e}"))?,
            private,
        })
    }

    /// The one `authorized_keys` line that names this key — what a test stages
    /// into the image so the guest will let it in.
    pub fn authorized_line(&self) -> String {
        self.line.clone()
    }

    /// The fingerprint the guest's daemon prints for this key.
    pub fn fingerprint(&self) -> &str {
        self.fingerprint.trim()
    }
}

/// What one `exec` came back with.
#[derive(Debug)]
pub struct Exec {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// `None` is a channel that closed without an `exit-status` message, which
    /// is itself a finding: a harness that cannot learn a program's status has
    /// no verdict to report, and one that read a missing status as zero would
    /// report a pass.
    pub status: Option<u32>,
}

impl Exec {
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// Run `command` on the guest and collect what it said and how it ended.
pub fn ssh_exec(
    host: &str,
    port: u16,
    identity: &Identity,
    command: &str,
) -> Result<Exec, String> {
    let (out, err, port) = capture(identity, port)?;
    let said = client(&["exec", host, &port, str(&identity.private), str(&out), str(&err), command])?;
    collected(&said, &out, &err)
}

/// Run `command` with `stdin` on its input, after asking the guest to set an
/// environment variable. `Ok`'s second half is what it answered that request.
pub fn ssh_feed(
    host: &str,
    port: u16,
    identity: &Identity,
    command: &str,
    stdin: &[u8],
) -> Result<(Exec, String), String> {
    let (out, err, port) = capture(identity, port)?;
    let local = out.with_file_name("stdin");
    std::fs::write(&local, stdin).map_err(|e| format!("stage {}: {e}", local.display()))?;
    let said = client(&[
        "feed",
        host,
        &port,
        str(&identity.private),
        str(&out),
        str(&err),
        str(&local),
        command,
    ])?;
    let env = said
        .lines()
        .find_map(|line| line.strip_prefix("env "))
        .ok_or_else(|| format!("the client said nothing about the env request: {said:?}"))?
        .to_string();
    Ok((collected(&said, &out, &err)?, env))
}

/// Start `command` on the guest and drop the connection once it is running.
pub fn ssh_abandon(
    host: &str,
    port: u16,
    identity: &Identity,
    command: &str,
) -> Result<(), String> {
    let port = port.to_string();
    client(&["abandon", host, &port, str(&identity.private), command])?;
    Ok(())
}

/// Where one exchange's two captured streams go, and the port as the argv
/// wants it.
fn capture(identity: &Identity, port: u16) -> Result<(PathBuf, PathBuf, String), String> {
    let dir = identity.private.with_extension(format!("exec-{}", nonce()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    Ok((dir.join("stdout"), dir.join("stderr"), port.to_string()))
}

/// The client's last line is the program's status; the captures beside it are
/// the bytes it wrote on each stream.
fn collected(said: &str, out: &Path, err: &Path) -> Result<Exec, String> {
    let last = said.lines().last().unwrap_or("").trim();
    let status = match last {
        "no-exit-status" => None,
        line => match line.strip_prefix("exit ") {
            Some(code) => {
                Some(code.parse().map_err(|_| format!("the client answered {said:?}"))?)
            }
            None => return Err(format!("the client answered {said:?}")),
        },
    };
    Ok(Exec {
        stdout: std::fs::read(out).map_err(|e| format!("read the captured stdout: {e}"))?,
        stderr: std::fs::read(err).map_err(|e| format!("read the captured stderr: {e}"))?,
        status,
    })
}

/// Put `bytes` on the guest at `remote`.
pub fn ssh_put(
    host: &str,
    port: u16,
    identity: &Identity,
    remote: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let local = identity.private.with_extension(format!("put-{}", nonce()));
    std::fs::write(&local, bytes).map_err(|e| format!("stage {}: {e}", local.display()))?;
    let port = port.to_string();
    client(&["put", host, &port, str(&identity.private), str(&local), remote])?;
    Ok(())
}

/// Read the guest's `remote` onto the host.
pub fn ssh_get(
    host: &str,
    port: u16,
    identity: &Identity,
    remote: &str,
) -> Result<Vec<u8>, String> {
    let local = identity.private.with_extension(format!("get-{}", nonce()));
    let _ = std::fs::remove_file(&local);
    let port = port.to_string();
    client(&["get", host, &port, str(&identity.private), remote, str(&local)])?;
    std::fs::read(&local).map_err(|e| format!("read what the client fetched: {e}"))
}

/// The guest's listing of `remote`, one `<name> <size>` per entry, sorted.
pub fn ssh_list(
    host: &str,
    port: u16,
    identity: &Identity,
    remote: &str,
) -> Result<Vec<String>, String> {
    let port = port.to_string();
    let said = client(&["list", host, &port, str(&identity.private), remote])?;
    Ok(said
        .lines()
        .filter_map(|line| line.strip_prefix("entry ").map(str::to_string))
        .collect())
}

/// What offering an unauthorized key came back with.
pub struct Refusal {
    /// Whether the guest answered the offer with `USERAUTH_PK_OK` — asking a
    /// stranger to sign, rather than refusing at the probe.
    pub asked_to_sign: bool,
    /// The comma-separated methods the guest *still* offers after the refusal:
    /// the answer to "what could a client guess at instead", and the only
    /// place the daemon's `MethodSet` is visible from outside it.
    pub methods: String,
}

/// Offer this key and expect the guest to turn it away. An error is the
/// finding: either the connection did not happen at all — which says nothing
/// about authentication — or the guest let in a key no file names.
pub fn ssh_refused(host: &str, port: u16, identity: &Identity) -> Result<Refusal, String> {
    let port = port.to_string();
    let said = client(&["auth", host, &port, str(&identity.private)])?;
    let asked_to_sign = match said.lines().find_map(|l| l.strip_prefix("signed ")) {
        Some("yes") => true,
        Some("no") => false,
        other => return Err(format!("the client said {other:?} about signing")),
    };
    let last = said.lines().last().unwrap_or("").trim();
    let methods = match last {
        "authenticated" => {
            return Err(format!(
                "{host}:{port} authenticated a key no authorized_keys file on it names"
            ));
        }
        "asked to sign" => String::new(),
        line => match line.strip_prefix("refused offering ") {
            Some(methods) => methods.to_string(),
            None => return Err(format!("the client answered {line:?}")),
        },
    };
    Ok(Refusal { asked_to_sign, methods })
}

/// Run the client and hand back what it said, or the reason it could not say
/// anything. Its last line is the answer; the ones before it, if any, are a
/// listing's entries.
fn client(argv: &[&str]) -> Result<String, String> {
    // Spelled in one expression because `src/sourcegate.rs` reads the argument
    // text: every host binary this project runs is declared beside the reason,
    // and a path bound to a name first would reach that scan as nothing.
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

/// A per-call suffix, so two calls of one test do not read each other's capture.
fn nonce() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

// --- The gate: one boot of `tests/sshdcase`, three judges on it ---

/// The name the boot's staged key is minted under. One name, so every judge on
/// this boot offers the key its image authorizes.
pub const KEY: &str = "sshdcase";

/// A second key, minted and staged nowhere. The whole of the negative arm: a
/// well-formed offer from a key no file names.
pub const STRANGER_KEY: &str = "sshdcase-stranger";

/// Where the image's `authorized_keys` file lands, ROOT-relative — the guest
/// reads it at `/system/etc/ssh_authorized_keys`, which `userland/sshd`'s
/// `AUTHORIZED_KEYS` is the other half of.
const KEYS_ON_ROOT: &str = "etc/ssh_authorized_keys";
const KEYS_IN_GUEST: &str = "/system/etc/ssh_authorized_keys";

/// The guest test binary run over `exec`. Self-contained — `/tmp` and syscalls,
/// no capability its spawner has to hand it — and it cleans up after itself so
/// the boot's later judges see the `/tmp` they would have seen.
const GUEST_TEST: &str = "test_rs_empty_dir_stat";

/// A path nothing on the guest has, for the arm that reads a program's stderr.
const MISSING: &str = "/tmp/no_such_file_for_the_stderr_arm";

/// `tests/sshdcase` with a key in its image and a forward into its port 22.
///
/// **The key is staged rather than installed**: `/home` on this machine may be
/// a tmpfs, so a key that had to be put there after the boot is a key nobody
/// could put there.
///
/// Panics rather than failing a test on any of the three things below: a
/// profile with no NIC, an argv with no forward, and a daemon that never opened
/// its port are each a machine the gate cannot run on at all, which is the same
/// class as a guest that never printed its ready marker.
pub fn boot(rust_bins: &[(String, Vec<u8>)]) -> super::qemu::QemuInstance {
    let identity = Identity::mint(KEY).unwrap_or_else(|why| panic!("[sshd] {why}"));
    let options = super::qemu::BootOptions {
        profile: super::qemu::Profile::Headless,
        extra_root_files: vec![(
            KEYS_ON_ROOT.to_string(),
            identity.authorized_line().into_bytes(),
        )],
        ssh_port: Some(super::qemu::free_host_port()),
        ..Default::default()
    };
    // Asked of the argv this boot is about to use, not assumed: without a NIC
    // the daemon leaves at its bind, and without the forward nothing on this
    // host can open a connection into the guest — either way every judge below
    // would fail for a reason that has nothing to do with sshd.
    let argv = super::qemu::profile_argv(&options);
    let forward = super::qemu::ssh_forward_argv(options.ssh_port.expect("just set"));
    assert!(
        argv.iter().any(|a| a.contains("virtio-net")),
        "[sshd] this gate needs a NIC and the profile carries none"
    );
    assert!(
        argv.iter().any(|a| a.contains(&forward)),
        "[sshd] the argv carries no {forward}, so nothing on this host can reach the guest"
    );

    let config = compile::repo_root().join("tests/sshdcase");
    let mut guest =
        super::qemu::QemuInstance::boot_with_options(&config, &[], rust_bins, options);
    let mut console = guest.boot_log().to_string();
    if let Err(why) = super::qemu::await_marker(
        &mut guest,
        &mut console,
        "sshd: listening on port 22",
        "sshd to open its port",
    ) {
        panic!("[sshd] never listened, so no exchange below would mean anything: {why}\n{console}");
    }
    guest
}

/// What `exec` is for: run this program, and tell me how it ended.
pub fn exec_gate(guest: &mut super::qemu::QemuInstance) -> Result<(), String> {
    let identity = Identity::mint(KEY)?;
    let port = guest.ssh_port();

    // 1. A program that runs, its output byte-exact and its status zero.
    let echo = ssh_exec(HOST, port, &identity, "echo hello from toyos")?;
    if echo.stdout != b"hello from toyos\n" {
        return Err(format!("`echo` answered {:?}", echo.stdout_text()));
    }
    if !echo.stderr.is_empty() {
        return Err(format!("`echo` wrote {:?} to the channel's stderr", echo.stderr_text()));
    }
    if echo.status != Some(0) {
        return Err(format!("`echo` ended {:?}", echo.status));
    }

    // 2. A program that is not there. The refusal is named on stderr and
    //    carried in the status; what it must never be is a hang or a silent
    //    zero, which is the whole reason a harness can trust arm 3.
    let missing = ssh_exec(HOST, port, &identity, "no_such_program_on_this_machine")?;
    if missing.status != Some(127) {
        return Err(format!(
            "a missing program ended {:?}, not 127:\n{}",
            missing.status,
            missing.stderr_text()
        ));
    }
    if !missing.stderr_text().contains("cannot run /system/bin/no_such_program_on_this_machine") {
        return Err(format!("the refusal names nothing: {:?}", missing.stderr_text()));
    }
    if !missing.stdout.is_empty() {
        return Err(format!("a refused exec wrote {:?} to stdout", missing.stdout_text()));
    }

    // 3. A line the daemon will not read as a command at all, refused before
    //    anything is spawned.
    let unquoted = ssh_exec(HOST, port, &identity, "echo 'unterminated")?;
    if unquoted.status != Some(127) || !unquoted.stderr_text().contains("unterminated ' quote") {
        return Err(format!(
            "an unquotable line ended {:?} saying {:?}",
            unquoted.status,
            unquoted.stderr_text()
        ));
    }

    // 4. A real guest test binary, run over the cable and judged by its exit
    //    status.
    let gate = ssh_exec(HOST, port, &identity, GUEST_TEST)?;
    if gate.status != Some(0) {
        return Err(format!(
            "{GUEST_TEST} ended {:?} over ssh:\n{}\n{}",
            gate.status,
            gate.stdout_text(),
            gate.stderr_text()
        ));
    }
    if !gate.stdout_text().contains("empty dir stat:") {
        return Err(format!("{GUEST_TEST} printed {:?}", gate.stdout_text()));
    }

    // 5. **The two streams are two streams.** A program that writes to both:
    //    stdout carries the file, stderr the diagnostic, and neither carries
    //    the other's bytes. Merging stderr into stdout — which is what this
    //    daemon used to do — is seen here and nowhere else.
    let both = ssh_exec(HOST, port, &identity, &format!("cat {KEYS_IN_GUEST} {MISSING}"))?;
    if both.stdout != identity.authorized_line().into_bytes() {
        return Err(format!("stdout carried {:?}", both.stdout_text()));
    }
    if !both.stderr_text().contains(&format!("{MISSING}: file not found")) {
        return Err(format!("stderr carried {:?}", both.stderr_text()));
    }
    if both.status != Some(1) {
        return Err(format!("a program that wrote to both ended {:?}", both.status));
    }

    // 6. A program's input is the channel's data, and an `env` request is
    //    answered rather than left for a client to wait on.
    let (fed, env) = ssh_feed(HOST, port, &identity, "cat", b"the input arrives\n")?;
    if fed.stdout != b"the input arrives\n" || fed.status != Some(0) {
        return Err(format!("`cat` of the channel's input said {:?}", fed.stdout_text()));
    }
    if env != "refused" {
        return Err(format!("the guest answered an env request {env:?}"));
    }

    // 7. A program that never exits, on a connection that goes away. Nothing
    //    is left running on the machine, and the daemon names what it ended.
    let mut console = String::new();
    ssh_abandon(HOST, port, &identity, "spin")?;
    super::qemu::await_marker(
        guest,
        &mut console,
        "the connection is gone; ended /system/bin/spin",
        "sshd to end a program whose connection went",
    )
    .map_err(|e| format!("a program outlived the connection that started it: {e}\n{console}"))?;

    eprintln!(
        "  [sshd] echo, a missing program (127), an unquotable line (127), {GUEST_TEST} (0), \
         the two streams apart, the channel's input read, and a spin ended with its connection"
    );
    Ok(())
}

/// A file in and a file out, judged by its bytes on this host.
pub fn files_gate(guest: &mut super::qemu::QemuInstance) -> Result<(), String> {
    let identity = Identity::mint(KEY)?;
    let port = guest.ssh_port();

    // 1. A file this host already knows every byte of, read off the guest.
    //    **It never travelled over SFTP** — the build wrote it into the image —
    //    so a read path that quietly reordered or padded is a disagreement
    //    here rather than a round trip agreeing with itself.
    let staged = ssh_get(HOST, port, &identity, KEYS_IN_GUEST)?;
    if staged != identity.authorized_line().into_bytes() {
        return Err(format!(
            "{KEYS_IN_GUEST} came back as {} bytes and the build wrote {}",
            staged.len(),
            identity.authorized_line().len()
        ));
    }

    // 2. Out and back, byte for byte, on a small file with every byte value in
    //    it — a transfer that is text-safe and nothing else passes this.
    let small: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
    ssh_put(HOST, port, &identity, "/tmp/ssh_small", &small)?;
    let back = ssh_get(HOST, port, &identity, "/tmp/ssh_small")?;
    if back != small {
        return Err(format!(
            "a 1,000-byte file came back as {} bytes, first difference at {:?}",
            back.len(),
            back.iter().zip(&small).position(|(a, b)| a != b)
        ));
    }

    // 3. The guest's own stat of what it was given, so the size is two
    //    readings and not one.
    let listing = ssh_list(HOST, port, &identity, "/tmp")?;
    if !listing.iter().any(|entry| entry == "ssh_small 1000") {
        return Err(format!("the guest lists /tmp as {listing:?}"));
    }

    // 4. A megabyte each way: several SFTP requests, several channel windows,
    //    and a guest that has to keep its place across all of them.
    let big = pseudorandom(1 << 20);
    ssh_put(HOST, port, &identity, "/tmp/ssh_big", &big)?;
    let back = ssh_get(HOST, port, &identity, "/tmp/ssh_big")?;
    if back != big {
        return Err(format!(
            "a 1 MiB file came back as {} bytes, first difference at {:?}",
            back.len(),
            back.iter().zip(&big).position(|(a, b)| a != b)
        ));
    }

    eprintln!(
        "  [sshd] the staged file read back byte-exact, and 1,000 B and 1 MiB moved both ways"
    );
    Ok(())
}

/// Who the machine lets in, in both directions.
pub fn key_auth_gate(guest: &mut super::qemu::QemuInstance) -> Result<(), String> {
    let identity = Identity::mint(KEY)?;
    let stranger = Identity::mint(STRANGER_KEY)?;
    let port = guest.ssh_port();
    let mut console = String::new();

    // The negative arm. A second connection, a well-formed offer, and a key no
    // file on the machine names.
    //
    // **It is refused at the probe.** A public-key exchange is an offer with no
    // signature and then, only under `USERAUTH_PK_OK`, a signature; a machine
    // that answers `PK_OK` to a stranger has told it the key would be taken and
    // asked it to prove it holds it. The client here cannot sign, so being
    // asked at all is the finding.
    //
    // What the machine still offers after the refusal is the other half: the
    // daemon narrows russh's `MethodSet` to public keys alone, and one that
    // offered `password` or `keyboard-interactive` here would be offering a
    // credential to guess at. This is the only place that narrowing is visible
    // from outside the daemon.
    let refusal = ssh_refused(HOST, port, &stranger)?;
    if refusal.asked_to_sign {
        return Err("the machine asked a key no file names to sign, so it answered PK_OK to a \
                    stranger's offer instead of refusing it"
            .to_string());
    }
    if refusal.methods != "publickey" {
        return Err(format!(
            "after refusing a key the machine still offers {:?}, not publickey alone",
            refusal.methods
        ));
    }
    super::qemu::await_marker(
        guest,
        &mut console,
        &format!("{} is authorized by no file, and was not asked to sign", stranger.fingerprint()),
        "sshd to name the key it refused at the offer",
    )
    .map_err(|e| format!("sshd refused a key without saying which: {e}\n{console}"))?;

    // And the positive one, said the same way: the key the image authorizes is
    // named on the console as the one that got in. Without this arm a daemon
    // that refused *everything* would pass the arm above.
    let ok = ssh_exec(HOST, port, &identity, "echo in")?;
    if ok.status != Some(0) || ok.stdout != b"in\n" {
        return Err(format!("the authorized key got {:?} / {:?}", ok.status, ok.stdout_text()));
    }
    super::qemu::await_marker(
        guest,
        &mut console,
        &format!("root authenticated with {}", identity.fingerprint()),
        "sshd to name the key it accepted",
    )
    .map_err(|e| format!("sshd accepted a key without saying which: {e}\n{console}"))?;

    eprintln!(
        "  [sshd] {} accepted and {} refused at the offer without being asked to sign, each \
         named on the console, and {} the only method left to try",
        identity.fingerprint(),
        stranger.fingerprint(),
        refusal.methods
    );
    Ok(())
}

/// A megabyte no compressor shortens and no run-length check passes by
/// accident. A 64-bit LCG, so the host and nothing else decides the bytes.
fn pseudorandom(len: usize) -> Vec<u8> {
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u8
        })
        .collect()
}
