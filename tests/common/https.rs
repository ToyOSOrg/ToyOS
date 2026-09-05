//! The `https_tls13` judge: `tests/https-server-host` on the host, the guest's
//! own `https_fetch` against it, and the same program built by the host's std
//! as the differential arm.
//!
//! The two arms fetch the same body from the same port and their printed lines
//! are compared whole, so a ToyOS socket that truncates, reorders or duplicates
//! is a disagreement rather than a smaller number nobody reads.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use super::compile;
use super::qemu::{self, BootOptions, QemuInstance};

/// Where the guest sees the host under QEMU's user-mode networking, and where
/// the host arm sees the same servers. The judge's certificate carries both.
const GUEST_VIEW_OF_HOST: &str = "10.0.2.2";
const HOST_VIEW_OF_HOST: &str = "127.0.0.1";

/// Where the minted CA lands on ROOT, which mounts at `/system`.
const CA_ON_ROOT: &str = "etc/https-judge-ca.pem";
const CA_IN_GUEST: &str = "/system/etc/https-judge-ca.pem";

/// Every refusal arm: the port role the server prints, and the one line the
/// client must answer with. `ok` is elsewhere — its line carries the digest.
const REFUSALS: &[(&str, &str)] = &[
    ("wrongname", "https_fetch: refused hostname-mismatch"),
    ("expired", "https_fetch: refused certificate-expired"),
    ("tls12", "https_fetch: refused tls12-refused"),
    ("downgrade", "https_fetch: refused downgrade-refused"),
    // A valid TLS server whose `302` names a cleartext URL: the peer chooses
    // the second hop, so only `https_only` stands between it and plaintext.
    ("redirect", "https_fetch: refused plain-http"),
];

pub fn tls13_judge(rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let bins: Vec<(String, Vec<u8>)> = rust_bins
        .iter()
        .filter(|(name, _)| name == "https_fetch")
        .cloned()
        .collect();
    if bins.is_empty() {
        return Err("https_fetch was not built".to_string());
    }

    let server = Server::start()?;
    let ca = std::fs::read(&server.ca)
        .map_err(|e| format!("read the minted CA {}: {e}", server.ca.display()))?;

    // Headless is the profile with virtio-net, and tests/netcase the only
    // config that puts netd in front of one.
    let options = BootOptions {
        profile: qemu::Profile::Headless,
        extra_root_files: vec![(CA_ON_ROOT.to_string(), ca)],
        ..Default::default()
    };
    if !qemu::profile_argv(&options).iter().any(|a| a.contains("virtio-net")) {
        return Err("this test needs a NIC and the profile has none".to_string());
    }
    let config = compile::repo_root().join("tests/netcase");
    let mut guest = QemuInstance::boot_with_options(&config, &[], &bins, options);
    let mut console = guest.boot_log().to_string();
    super::qemu::await_marker(
        &mut guest,
        &mut console,
        "netd: ready, at most ",
        "netd to come up",
    )
    .map_err(|e| format!("netd never came up, so no fetch below means anything: {e}"))?;

    let ok_line = format!(
        "https_fetch: ok bytes={} sha256={}",
        server.body_bytes, server.body_sha
    );
    let good = server.port("ok")?;
    let mut lines = Vec::new();

    let guest_ok = fetch_in_guest(&mut guest, GUEST_VIEW_OF_HOST, good, true)?;
    if guest_ok != ok_line {
        return Err(format!("the guest fetched {guest_ok:?}, and the server served {ok_line:?}"));
    }
    lines.push(format!("ok: {guest_ok}"));

    for (role, expected) in REFUSALS {
        let port = server.port(role)?;
        let got = fetch_in_guest(&mut guest, GUEST_VIEW_OF_HOST, port, true)?;
        if got != *expected {
            return Err(format!("the {role} arm answered {got:?}, not {expected:?}"));
        }
        lines.push(format!("{role}: {got}"));
    }

    // The CA is what makes the judge's own roots trusted, so withholding it is
    // the unknown-authority arm rather than a separate server.
    let unknown = fetch_in_guest(&mut guest, GUEST_VIEW_OF_HOST, good, false)?;
    if unknown != "https_fetch: refused unknown-authority" {
        return Err(format!("a fetch with no extra root answered {unknown:?}"));
    }
    lines.push(format!("unknown-authority: {unknown}"));

    let cleartext = server.port("plain")?;
    let plain = run_guest(
        &mut guest,
        &format!("test_rs_https_fetch http://{GUEST_VIEW_OF_HOST}:{cleartext}/ --ca {CA_IN_GUEST}"),
    )?;
    if plain != "https_fetch: refused plain-http" {
        return Err(format!("a plain http:// fetch answered {plain:?}"));
    }
    lines.push(format!("plain-http: {plain}"));

    let host_ok = fetch_on_host(&format!(
        "https://{HOST_VIEW_OF_HOST}:{good}/"
    ), &server.ca)?;
    if host_ok != guest_ok {
        return Err(format!(
            "the differential arms disagree: ToyOS answered {guest_ok:?} and the host's own std \
             answered {host_ok:?} for the same body on the same port"
        ));
    }

    for line in &lines {
        eprintln!("  [https] {line}");
    }
    eprintln!("  [https] host arm agreed byte for byte: {host_ok}");
    Ok(())
}

fn fetch_in_guest(
    guest: &mut QemuInstance,
    host: &str,
    port: u16,
    with_ca: bool,
) -> Result<String, String> {
    let ca = if with_ca { format!(" --ca {CA_IN_GUEST}") } else { String::new() };
    run_guest(guest, &format!("test_rs_https_fetch https://{host}:{port}/{ca}"))
}

fn run_guest(guest: &mut QemuInstance, command: &str) -> Result<String, String> {
    let result = guest.run_test(command, Duration::from_secs(120));
    if let Some(err) = &result.error {
        return Err(format!("{command}: {err}\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!(
            "{command} exited {:?}:\n{}",
            result.exit_code, result.stdout
        ));
    }
    answer(&result.stdout).ok_or_else(|| format!("{command} printed no verdict:\n{}", result.stdout))
}

/// The one line the program prints, out of a capture that may carry a daemon's.
fn answer(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .find(|l| l.contains("https_fetch: "))
        .map(|l| l[l.find("https_fetch: ").expect("just matched")..].trim_end().to_string())
}

fn fetch_on_host(url: &str, ca: &Path) -> Result<String, String> {
    let out = Command::new(toyos_build::build::https_fetch_host(&compile::repo_root()))
        .args([url, "--ca"])
        .arg(ca)
        .output()
        .map_err(|e| format!("run the host arm: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    answer(&stdout).ok_or_else(|| format!("the host arm printed no verdict:\n{stdout}"))
}

/// The host servers, killed when this goes out of scope.
struct Server {
    child: Child,
    ca: PathBuf,
    body_bytes: usize,
    body_sha: String,
    ports: BTreeMap<String, u16>,
}

impl Server {
    fn start() -> Result<Self, String> {
        let out = super::lane::dir().join("https-judge");
        std::fs::create_dir_all(&out).map_err(|e| format!("create {}: {e}", out.display()))?;
        let mut child = Command::new(toyos_build::build::https_test_server(&compile::repo_root()))
            .arg("--out")
            .arg(&out)
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("start the judge's servers: {e}"))?;

        let stdout = child.stdout.take().expect("a piped stdout");
        let mut ca = None;
        let mut body_bytes = None;
        let mut body_sha = None;
        let mut ports = BTreeMap::new();
        // The server binds every listener before it prints `ready`, so nothing
        // below races an accept loop that does not exist yet.
        for line in BufReader::new(stdout).lines() {
            let line = line.map_err(|e| format!("read the judge's contract: {e}"))?;
            let mut field = line.split_whitespace();
            match (field.next(), field.next(), field.next()) {
                (Some("ca"), Some(path), None) => ca = Some(PathBuf::from(path)),
                (Some("body-bytes"), Some(n), None) => body_bytes = n.parse().ok(),
                (Some("body-sha256"), Some(hex), None) => body_sha = Some(hex.to_string()),
                (Some("port"), Some(role), Some(n)) => {
                    if let Ok(port) = n.parse() {
                        ports.insert(role.to_string(), port);
                    }
                }
                (Some("ready"), None, None) => break,
                _ => return Err(format!("the judge's servers said {line:?}")),
            }
        }

        let (Some(ca), Some(body_bytes), Some(body_sha)) = (ca, body_bytes, body_sha) else {
            let _ = child.kill();
            return Err("the judge's servers never announced a CA and a body".to_string());
        };
        Ok(Server { child, ca, body_bytes, body_sha, ports })
    }

    fn port(&self, role: &str) -> Result<u16, String> {
        self.ports
            .get(role)
            .copied()
            .ok_or_else(|| format!("the judge's servers opened no {role} port"))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
