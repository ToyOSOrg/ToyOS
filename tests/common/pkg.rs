//! `pkg install <file>` from a local archive, and gbae's first run on ToyOS.
//!
//! The subject is the whole package path: the release's own `SHA256SUMS`
//! decides whether an archive is installed at all, `/apps/gbae` is a directory
//! and nothing else, and what a launch out of that directory *holds* comes from
//! the image's `[apps]` row rather than from the caller. `tests/pkgcase` is
//! what makes the last of those checkable — the estate that launches gbae has
//! no `compositor` connector of its own, so a window is proof the row was
//! built.
//!
//! Two oracles, neither of them this file's: the release's `SHA256SUMS`, which
//! a third party wrote and the guest verifies against; and the `tar` crate,
//! which decodes the same archive on the host so the bytes read back off the
//! guest's DATA volume are compared with a decoder `userland/pkg` shares no
//! code with.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::qemu::{self, BootOptions, QemuInstance};
use super::storage::{superblock_at, FileBlocks};

/// gbae v0.2.0's release, pinned.
const RELEASE: &str = "https://github.com/Japabu/gbae/releases/download/v0.2.0";
const ASSET: &str = "gbae-v0.2.0-toyos-x86_64.tar.gz";
const SUMS: &str = "SHA256SUMS";

/// The release's own line for [`ASSET`], copied from its `SHA256SUMS`. The
/// fetch below is verified against it, so a mirror, a proxy or a re-cut release
/// is refused on the host before the guest is handed anything.
const ASSET_SHA256: &str = "99fcd8a7263b5c25cd90cead1baaa7200ef272100fc2226e008a4e8205ba2916";
const ASSET_BYTES: usize = 604_872;

/// Where the archive and its two negative controls sit on ROOT.
const GOOD_DIR: &str = "share/pkg";
const TAMPERED_DIR: &str = "share/pkg/tampered";
const NOSUMS_DIR: &str = "share/pkg/nosums";

/// What a package's directory holds after this archive is installed.
const INSTALLED: [&str; 4] = ["gbae", "LICENSE", "README.md", "manifest.toml"];

pub fn pkg_install_gbae(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let (archive, sums) = fixture()?;
    let mut tampered = archive.clone();
    // One byte, in the middle of the compressed stream: the digest is what
    // must refuse it, and a gzip that also fails to inflate would let the
    // wrong refusal pass for the right one.
    tampered[ASSET_BYTES / 2] ^= 0x01;

    let bins: Vec<(String, Vec<u8>)> =
        rust_bins.iter().filter(|(name, _)| name == "pkg_launch_gbae").cloned().collect();
    if bins.is_empty() {
        return Err(String::from("the pkg_launch_gbae client was not built"));
    }

    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pkgcase");
    let options = BootOptions {
        profile: qemu::Profile::Metal,
        extra_root_files: vec![
            (format!("{GOOD_DIR}/{ASSET}"), archive.clone()),
            (format!("{GOOD_DIR}/{SUMS}"), sums.clone().into_bytes()),
            (format!("{TAMPERED_DIR}/{ASSET}"), tampered),
            (format!("{TAMPERED_DIR}/{SUMS}"), sums.into_bytes()),
            (format!("{NOSUMS_DIR}/{ASSET}"), archive.clone()),
        ],
        ..Default::default()
    };
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &bins, options);
    let boot = qemu.boot_log().to_string();
    if boot.contains("are a tmpfs") {
        return Err(format!(
            "/apps and /home fell back to tmpfs, so the readback below would judge no device:\n\
             {boot}"
        ));
    }

    let mut log = boot;
    guest_probes(&mut qemu, &mut log)?;

    let image = qemu.nvme_image().to_path_buf();
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let tail = qemu.drain_serial(Duration::from_secs(20));
    drop(qemu);
    for bad in ["PANIC:", "panicked at"] {
        if tail.contains(bad) {
            return Err(format!("{bad:?} on the way down\n{tail}"));
        }
    }

    readback(&image, &archive)
}

/// Every claim the guest can answer for, in the order that makes each one
/// mean something.
fn guest_probes(qemu: &mut QemuInstance, log: &mut String) -> Result<(), String> {
    let good = format!("/system/{GOOD_DIR}/{ASSET}");

    // The two refusals first, and by name: a tampered archive beside a sums
    // file that covers the real one, and a real archive with no sums file
    // beside it at all.
    refused(
        qemu,
        log,
        &format!("pkg install /system/{TAMPERED_DIR}/{ASSET} --yes"),
        &format!("pkg: {ASSET} hashes to"),
    )?;
    refused(
        qemu,
        log,
        &format!("pkg install /system/{NOSUMS_DIR}/{ASSET} --yes"),
        &format!("pkg: cannot read /system/{NOSUMS_DIR}/{SUMS}"),
    )?;

    // Nothing is installed, so init has no row to build and the launch is
    // refused rather than falling back to the caller's own namespace.
    let at = log.len();
    refused(qemu, log, "test_rs_pkg_launch_gbae", "did not start")?;
    const WHY: &str = "init: launcher: /apps/gbae/manifest.toml cannot be read";
    if !log[at.min(log.len())..].contains(WHY) {
        return Err(format!("init never said {WHY:?}:\n{}", &log[at.min(log.len())..]));
    }

    // Consent: `test-runner` closes a child's stdin, so this asks and is
    // answered with nothing.
    refused(qemu, log, &format!("pkg install {good}"), "pkg: not installing gbae")?;

    let installed = passed(qemu, log, &format!("pkg install {good} --yes"))?;
    for said in [
        format!("pkg: verified {ASSET} against {SUMS} ({ASSET_SHA256})"),
        String::from("pkg: installed gbae 0.2.0 at /apps/gbae, launching /apps/gbae/gbae"),
    ] {
        if !installed.contains(&said) {
            return Err(format!("no {said:?} line:\n{installed}"));
        }
    }

    let listed = passed(qemu, log, "pkg list")?;
    let row = format!("gbae 0.2.0 /apps/gbae/gbae {ASSET_SHA256}");
    if !listed.contains(&row) {
        return Err(format!("`pkg list` does not carry {row:?}:\n{listed}"));
    }

    // Removal is deleting the directory, judged by the name coming free: a
    // second install of the same archive is refused while `/apps/gbae` exists.
    passed(qemu, log, "pkg remove gbae")?;
    let empty = passed(qemu, log, "pkg list")?;
    if empty.contains("gbae 0.2.0") {
        return Err(format!("`pkg remove` left gbae in the listing:\n{empty}"));
    }
    refused(qemu, log, "test_rs_pkg_launch_gbae", "did not start")?;
    passed(qemu, log, &format!("pkg install {good} --yes"))?;

    // And the window. The estate that runs this holds no `compositor`
    // connector, so a census of one is the `[apps]` row and can be nothing
    // else.
    let opened = log.len();
    passed(qemu, log, "test_rs_pkg_launch_gbae")?;
    if !window_seen(qemu, log, opened) {
        return Err(format!(
            "gbae started and the compositor never counted a window:\n{}",
            &log[opened.min(log.len())..]
        ));
    }
    eprintln!("  [pkg] gbae opened a window through the /apps row alone");
    Ok(())
}

/// Wait for a compositor census carrying a window, past `from`.
///
/// The census is printed every `STATS_INTERVAL`, so this is a wait on the
/// compositor's own clock rather than a span of host wall clock: the ceiling
/// is a liveness guard and the `windows=1` field is the verdict.
fn window_seen(qemu: &mut QemuInstance, log: &mut String, from: usize) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        log.push_str(&qemu.drain_serial(Duration::from_millis(500)));
        if log[from.min(log.len())..]
            .lines()
            .any(|l| l.contains("compositor: frames=") && l.contains("windows=1"))
        {
            return true;
        }
    }
    false
}

/// Run one guest command that must succeed, answering its output.
fn passed(qemu: &mut QemuInstance, log: &mut String, command: &str) -> Result<String, String> {
    let result = qemu.run_test(command, Duration::from_secs(120));
    let output = format!("{}{}", result.stdout, result.serial);
    log.push_str(&result.before);
    log.push_str(&output);
    if result.exit_code != Some(0) {
        return Err(format!("`{command}` answered {:?}:\n{output}", result.exit_code));
    }
    Ok(output)
}

/// Run one guest command that must fail, and say so by name.
fn refused(
    qemu: &mut QemuInstance,
    log: &mut String,
    command: &str,
    says: &str,
) -> Result<(), String> {
    let result = qemu.run_test(command, Duration::from_secs(120));
    let output = format!("{}{}", result.stdout, result.serial);
    log.push_str(&result.before);
    log.push_str(&output);
    if result.exit_code == Some(0) {
        return Err(format!("`{command}` was not refused:\n{output}"));
    }
    if !output.contains(says) {
        return Err(format!("`{command}` was refused and never said {says:?}:\n{output}"));
    }
    Ok(())
}

/// What the guest wrote, read off the DATA volume with the guest gone.
///
/// The partition is found through the image's own table and the volume asked
/// which span it was formatted for, so nothing here takes an address from
/// anything the guest printed. Each file is compared with what the `tar` crate
/// makes of the same archive — a decoder `userland/pkg` shares no line with.
fn readback(image: &Path, archive: &[u8]) -> Result<(), String> {
    let (at, bytes) = toyos_build::image::data_partition_of(image)?;
    let blocks = bytes / 4096;
    let sb = superblock_at(image, at / 4096)?;
    if sb.block_count != blocks {
        return Err(format!(
            "the volume on the image was formatted for {} blocks and the DATA partition is \
             {blocks}",
            sb.block_count
        ));
    }

    let io = FileBlocks::open(image)?;
    let fs = bcachefs::Mounted::<_, bcachefs::ReadOnly>::open(io)
        .map_err(|e| format!("the NVMe image's DATA partition does not mount: {e:?}"))?;

    let mut total = 0usize;
    let mut found: Vec<String> = Vec::new();
    for (name, want) in third_party_entries(archive)? {
        let on_disk = format!("apps/{name}");
        let got = fs
            .read_file(&on_disk)
            .map_err(|e| format!("reading {on_disk} off the DATA partition: {e:?}"))?;
        if got != want {
            let first = got.iter().zip(&want).position(|(a, b)| a != b);
            return Err(format!(
                "{on_disk} is {} bytes on the device against the archive's {}, first differing \
                 at {first:?}",
                got.len(),
                want.len()
            ));
        }
        total += want.len();
        found.push(name.rsplit('/').next().unwrap_or(&name).to_string());
    }

    // The manifest is the installer's own and is in no archive, so it is
    // checked against the digest the release published rather than against a
    // file.
    let manifest = fs
        .read_file("apps/gbae/manifest.toml")
        .map_err(|e| format!("reading apps/gbae/manifest.toml off the DATA partition: {e:?}"))?;
    let text = String::from_utf8(manifest).map_err(|e| format!("the manifest is not UTF-8: {e}"))?;
    let want = format!(
        "name = \"gbae\"\nversion = \"0.2.0\"\ndigest = \"{ASSET_SHA256}\"\n\
         program = \"/apps/gbae/gbae\"\n"
    );
    if text != want {
        return Err(format!("the manifest on the device is {text:?}, not {want:?}"));
    }
    found.push(String::from("manifest.toml"));
    found.sort();
    let mut expected: Vec<&str> = INSTALLED.to_vec();
    expected.sort_unstable();
    if found != expected {
        return Err(format!("apps/gbae carries {found:?} and a package of this archive is \
                            {expected:?}"));
    }

    eprintln!(
        "  [pkg] {total} bytes of {ASSET} byte-identical under apps/gbae on the DATA partition \
         at byte {at}, against the `tar` crate's own decoding"
    );
    Ok(())
}

/// The archive's files, decoded by the `tar` crate rather than by
/// `userland/pkg`.
fn third_party_entries(archive: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let gz = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(gz);
    let mut out = Vec::new();
    for entry in tar.entries().map_err(|e| format!("tar: {e}"))? {
        let mut entry = entry.map_err(|e| format!("tar entry: {e}"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|e| format!("tar path: {e}"))?
            .to_string_lossy()
            .into_owned();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).map_err(|e| format!("tar read: {e}"))?;
        out.push((path, data));
    }
    if out.len() != 3 {
        return Err(format!("the archive holds {} files, and gbae v0.2.0 has 3", out.len()));
    }
    Ok(out)
}

/// The release asset and its sums file, fetched once per checkout.
///
/// **Nothing of it is committed** — no binary in this tree is — so it is kept
/// under `target/` in a directory named by the digest this file pins. A cache
/// that does not hash to that digest is a miss, so a tampered one can never be
/// a hit.
fn fixture() -> Result<(Vec<u8>, String), String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/pkg-fixtures")
        .join(ASSET_SHA256);
    let archive_at = dir.join(ASSET);
    let sums_at = dir.join(SUMS);
    if let (Ok(archive), Ok(sums)) =
        (std::fs::read(&archive_at), std::fs::read_to_string(&sums_at))
    {
        if digest(&archive) == ASSET_SHA256 {
            return Ok((archive, sums));
        }
    }

    let agent = agent();
    let archive = get(&agent, &format!("{RELEASE}/{ASSET}"))?;
    if archive.len() != ASSET_BYTES || digest(&archive) != ASSET_SHA256 {
        return Err(format!(
            "{RELEASE}/{ASSET} answered {} bytes hashing to {}, and this test pins {ASSET_BYTES} \
             bytes hashing to {ASSET_SHA256}",
            archive.len(),
            digest(&archive)
        ));
    }
    let sums = String::from_utf8(get(&agent, &format!("{RELEASE}/{SUMS}"))?)
        .map_err(|e| format!("{RELEASE}/{SUMS} is not UTF-8: {e}"))?;
    // The release has to say about itself what this file says about it, or the
    // guest would be verifying against a statement nobody checked.
    if !sums.contains(&format!("{ASSET_SHA256}  {ASSET}")) {
        return Err(format!("{RELEASE}/{SUMS} carries no `{ASSET_SHA256}  {ASSET}` line:\n{sums}"));
    }

    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    std::fs::write(&archive_at, &archive)
        .map_err(|e| format!("write {}: {e}", archive_at.display()))?;
    std::fs::write(&sums_at, &sums).map_err(|e| format!("write {}: {e}", sums_at.display()))?;
    Ok((archive, sums))
}

fn agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::Rustls)
        .root_certs(ureq::tls::RootCerts::WebPki)
        .unversioned_rustls_crypto_provider(std::sync::Arc::new(rustls_rustcrypto::provider()))
        .build();
    ureq::Agent::config_builder().tls_config(tls).build().new_agent()
}

fn get(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    agent
        .get(url)
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?
        .into_body()
        .into_reader()
        .read_to_end(&mut body)
        .map_err(|e| format!("reading {url}: {e}"))?;
    Ok(body)
}

fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}
