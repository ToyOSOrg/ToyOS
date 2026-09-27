//! The image release: the disk a person downloads and boots under QEMU or
//! writes to a stick, published by `cargo run -- --ci release`.
//!
//! **What is published is what booted**: the job builds `build::Boot::release`'s
//! image once, boots a copy of it under the Linux command line its notes print
//! ([`boots`]), and on `main` uploads those bytes. **Named by the commit**,
//! [`tag`]: every build draws its partition GUIDs and a runner mints its own
//! throwaway signing key (`src/signing.rs`), so a second build reproduces no
//! byte of the first. **Kept to [`KEEP`]**: each publish deletes every older
//! image release and its tag ([`stale`]).
//!
//! Not in `src/release.rs`, whose bytes are hashed into the toolchain's tag.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::arch::{Accel, Arch};
use crate::fingerprint::{first_difference, whole_device};
use crate::licence::{pending_owner, Subject};

/// What every image release's tag starts with; the rest is the commit's first
/// twelve hex digits.
pub const TAG_PREFIX: &str = "image-x86_64-";

/// How many image releases stay published.
pub const KEEP: usize = 7;

/// The compressed disk, as a release asset.
pub const IMAGE_ASSET: &str = "toyos.img.gz";

/// The image as the notes' commands name it once unpacked.
pub const IMAGE: &str = "toyos.img";

/// The SHA-256 of [`IMAGE_ASSET`], in the form `sha256sum -c` reads.
pub const SUMS_ASSET: &str = "SHA256SUMS";

/// The notes, also carried as an asset.
pub const NOTES_ASSET: &str = "README.md";

/// The image's licence notice (`build::RELEASE_NOTICES`), as an asset.
pub const LICENCES_ASSET: &str = "licences.txt";

/// One guest's ceiling from power-on to a painting desktop, unscaled: a
/// liveness guard, never a verdict.
pub const DESKTOP: Duration = Duration::from_secs(120);

/// How many releases one listing asks for; a listing that fills it is refused,
/// since what it left out cannot be judged.
const LISTED: usize = 1000;

/// `image-x86_64-<12 hex>` of a full commit id.
pub fn tag(commit: &str) -> Result<String, String> {
    let full = commit.len() == 40 && commit.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !full {
        return Err(format!("{commit:?} is not a full commit id"));
    }
    Ok(format!("{TAG_PREFIX}{}", &commit[..12]))
}

/// The hosts the notes give a command line for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    /// Homebrew's QEMU, emulating the x86-64 guest.
    MacosAppleSilicon,
    /// Debian's `qemu-system-x86` and `ovmf`, on the host's own CPU.
    LinuxKvm,
}

impl Host {
    pub const ALL: [Host; 2] = [Host::MacosAppleSilicon, Host::LinuxKvm];

    /// The one of the two this machine is, or, refused by name, that it is
    /// neither: the notes print no line for it, so there is no line to boot.
    pub fn this() -> Result<Host, String> {
        if cfg!(target_os = "macos") && Arch::HOST == Some(Arch::Aarch64) {
            return Ok(Host::MacosAppleSilicon);
        }
        if cfg!(target_os = "linux") && Arch::HOST == Some(Arch::X86_64) && Arch::X86_64.accel() == Accel::Kvm {
            return Ok(Host::LinuxKvm);
        }
        Err("the release notes print a command line for an Apple Silicon Mac and for an x86-64 \
             Linux whose /dev/kvm opens, and this host is neither"
            .into())
    }

    /// Where the notes say this host is.
    pub fn named(self) -> &'static str {
        match self {
            Host::MacosAppleSilicon => "macOS on Apple Silicon, with Homebrew's `qemu`",
            Host::LinuxKvm => {
                "Linux on x86-64 with KVM, with Debian's `qemu-system-x86` and `ovmf` and a user \
                 that can open `/dev/kvm`"
            }
        }
    }

    fn accel(self) -> Accel {
        match self {
            Host::MacosAppleSilicon => Accel::Tcg,
            Host::LinuxKvm => Accel::Kvm,
        }
    }

    /// The edk2 firmware the host's QEMU installation ships: its code, and the
    /// variable-store template beside it, which the guest gets read-only.
    pub fn firmware(self) -> [&'static str; 2] {
        match self {
            Host::MacosAppleSilicon => [
                "/opt/homebrew/share/qemu/edk2-x86_64-code.fd",
                "/opt/homebrew/share/qemu/edk2-i386-vars.fd",
            ],
            Host::LinuxKvm => ["/usr/share/OVMF/OVMF_CODE_4M.fd", "/usr/share/OVMF/OVMF_VARS_4M.fd"],
        }
    }

    /// The whole command line, program first, that boots `image` as a USB
    /// stick on the machine shape `cargo run` boots: q35 with a vIOMMU, a
    /// firmware framebuffer, a USB keyboard and tablet, and virtio-net. The
    /// kernel's log goes to the terminal.
    pub fn command(self, image: &str) -> Vec<String> {
        let accel = self.accel();
        let [code, vars] = self.firmware();
        [
            Arch::X86_64.qemu(),
            "-nodefaults",
            "-accel",
            accel.name(),
            "-cpu",
            Arch::X86_64.cpu(accel),
            "-machine",
            "q35,kernel-irqchip=split",
            "-smp",
            "4",
            "-m",
            "2G",
            "-drive",
            &format!("if=pflash,format=raw,unit=0,file={code},readonly=on"),
            "-drive",
            &format!("if=pflash,format=raw,unit=1,file={vars},readonly=on"),
            "-device",
            "intel-iommu,intremap=on,caching-mode=on,aw-bits=48",
            "-device",
            "nec-usb-xhci,id=xhci",
            "-drive",
            &format!("if=none,id=stick,format=raw,file={image}"),
            "-device",
            "usb-storage,bus=xhci.0,drive=stick,bootindex=0",
            "-device",
            "usb-kbd,bus=xhci.0",
            "-device",
            "usb-tablet,bus=xhci.0",
            "-vga",
            "std",
            "-netdev",
            "user,id=net0",
            "-device",
            "virtio-net-pci-non-transitional,netdev=net0,iommu_platform=on",
            "-serial",
            "stdio",
        ]
        .map(String::from)
        .to_vec()
    }
}

/// A QEMU started from a release command line, and its console so far.
struct Guest {
    child: Child,
    lines: Receiver<String>,
    log: String,
    stderr: PathBuf,
}

impl Guest {
    /// `argv` with no window: the notes' line, headless.
    fn start(argv: &[String], stderr: &Path) -> Result<Guest, String> {
        let err = fs::File::create(stderr).map_err(|e| format!("{}: {e}", stderr.display()))?;
        let arch = Arch::X86_64;
        if argv[0] != arch.qemu() {
            return Err(format!("a release command line starts {:?}, not {}", argv[0], arch.qemu()));
        }
        let mut child = Command::new(arch.qemu())
            .args(&argv[1..])
            .args(["-display", "none"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(err)
            .spawn()
            .map_err(|e| format!("{}: {e}", arch.qemu()))?;
        let out = child.stdout.take().expect("piped");
        let (send, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).split(b'\n') {
                let Ok(line) = line else { return };
                if send.send(String::from_utf8_lossy(&line).into_owned()).is_err() {
                    return;
                }
            }
        });
        Ok(Guest { child, lines, log: String::new(), stderr: stderr.to_path_buf() })
    }

    /// Read the console until every line of `want` is in it, within
    /// `ceiling`, refusing a panic the moment one is printed.
    fn until_said(&mut self, want: &[String], ceiling: Duration) -> Result<(), String> {
        let deadline = Instant::now() + ceiling;
        loop {
            if let Some(line) = self.log.lines().find(|l| l.contains("PANIC") || l.contains("panicked at")) {
                return Err(format!("the guest panicked before the desktop: {line}\n{}", self.log));
            }
            if want.iter().all(|line| self.log.contains(line.as_str())) {
                return Ok(());
            }
            match self.lines.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(line) => {
                    self.log.push_str(&line);
                    self.log.push('\n');
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!("no desktop within {ceiling:?}:\n{}", self.log));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let stderr = fs::read_to_string(&self.stderr).unwrap_or_default();
                    return Err(format!(
                        "QEMU closed its console before the desktop:\n{}\nQEMU's stderr:\n{stderr}",
                        self.log
                    ));
                }
            }
        }
    }
}

impl Drop for Guest {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Boot `stick` under `host`'s command line until the desktop paints: every
/// program `system.toml` starts said it started, the three that announce
/// themselves did, and the compositor reported a frame. And neither firmware
/// file changed, since the line gives the guest both read-only.
pub fn boots(root: &Path, host: Host, stick: &Path, ceiling: Duration, stderr: &Path) -> Result<(), String> {
    let firmware = host.firmware();
    if let Some(missing) = firmware.iter().find(|f| !Path::new(f).is_file()) {
        return Err(format!("{missing} is no file: this host lacks the firmware {host:?}'s line names"));
    }
    let before = firmware.map(|f| whole_device(Path::new(f)));
    let mut want: Vec<String> = crate::build::boot_start(&root.join("system.toml"))
        .iter()
        .map(|program| format!("init: started {program}"))
        .collect();
    want.extend(
        ["compositor: ready", "netd: ready", "logd: this boot's kernel log is", "compositor: frames="]
            .map(String::from),
    );
    let desktop = Guest::start(&host.command(&stick.display().to_string()), stderr)?.until_said(&want, ceiling);
    for (file, before) in firmware.iter().zip(&before) {
        if let Some(diff) = first_difference(before, &whole_device(Path::new(file))) {
            return Err(format!("the boot wrote {file}, which the line gives the guest read-only: {diff}"));
        }
    }
    desktop
}

/// [`Host::command`] as the notes print it: an option and its value to a line.
fn shell(argv: &[String]) -> String {
    let mut out = format!("    {}", argv[0]);
    for word in &argv[1..] {
        if word.starts_with('-') {
            out.push_str(" \\\n        ");
        } else {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// The floor variable a boot of the release leaves in the firmware, as the
/// notes name it: its name's fixed head, and how many hex digits follow.
fn floor_variable() -> (String, usize) {
    use toyos_update::floor::{Scope, NAME_BYTES, PREFIX};
    let head = format!("{PREFIX}-{}", Scope::Image.tag() as char);
    let hex = NAME_BYTES - head.len();
    (head, hex)
}

/// The release notes, which are also [`NOTES_ASSET`].
pub fn notes(root: &Path, tag: &str, commit: &str) -> Result<String, String> {
    let qemu = crate::ci::declared_qemu_version(root).ok_or(".github/qemu-version declares no version")?;
    let data = toyos_gpt::Guid::TOYOS_DATA_TEXT;
    let (floor, hex) = floor_variable();
    let vendor = toyos_update::floor::VENDOR;
    let on_root = crate::licence::NOTICES_ON_ROOT;
    let withheld: Vec<String> = pending_owner()
        .map(|s| match s {
            Subject::Crate(path) | Subject::Notice(path) | Subject::File(path) => format!("`{path}`"),
        })
        .collect();
    let mut commands = String::new();
    for host in Host::ALL {
        commands.push_str(&format!("{}:\n\n{}\n\n", host.named(), shell(&host.command(IMAGE))));
    }
    Ok(format!(
        "# ToyOS {tag}

The ToyOS disk image of commit {commit} on `main`. It booted to its desktop under the Linux command line below before it was published. It boots under QEMU, or from a USB stick on a UEFI x86-64 machine.

## Verify and unpack

Download `{IMAGE_ASSET}` and `{SUMS_ASSET}` from this release into one directory, and in it:

    sha256sum -c {SUMS_ASSET}
    gunzip {IMAGE_ASSET}

## Under QEMU

QEMU {qemu} is the version ToyOS is measured with. The firmware is edk2, from Homebrew's QEMU on macOS and from Debian's `ovmf` on Linux, and the guest gets both of its files read-only.

{commands}The kernel's log is on the terminal and the desktop is in QEMU's window. The running system writes to `{IMAGE}` itself, as it would to a stick. `/apps`, `/config`, `/home` and `/state` are kept in memory and are gone at the next boot.

## On a USB stick

The machine has to be an x86-64 PC from 2020 or later, booting UEFI with Secure Boot off. The only hardware ToyOS is known to work on is a Lenovo ThinkPad T14; on anything else it is untried.

Booting the stick writes two things into the machine's firmware:

- a variable named `{floor}` and {hex} hex digits, under the vendor GUID `{vendor}`: the loader's anti-rollback floor, eight bytes. It stays after the stick is gone. It is readable only before an operating system starts, so no operating system can see or remove it; only a UEFI shell can, with `dmpstore -d <its name> -guid {vendor}`. Booting another ToyOS image's stick replaces it rather than adding one;
- `BootNext`, where one of the machine's boot entries names the stick: the next restart boots the stick once.

Write `{IMAGE}` to the whole stick, not to a partition of it; what the stick held is lost. Unmount it first: on macOS `diskutil unmountDisk /dev/<the stick>`, on Linux `umount` each of its partitions that is mounted (`lsblk /dev/<the stick>` lists them). Then, as root:

    dd if={IMAGE} of=/dev/<the stick> bs=4194304
    sync

A boot writes to the stick it booted from, and to another disk only where that disk carries a partition of ToyOS's DATA type, `{data}`, a type no other system uses. That is where `/apps`, `/config`, `/home` and `/state` live. Every other disk is read for its partition table and never written.

## Terms

ToyOS is MIT OR Apache-2.0. `{LICENCES_ASSET}`, beside the image and on it at `/system/{on_root}`, gives every package the image is built from with its licence, where its source is and its licence texts, and every third-party file on the image with its terms and texts.

The image leaves out {withheld}, which the repository carries.
",
        withheld = withheld.join(", "),
    ))
}

/// The tags among `listed`, as (tag, creation time in RFC 3339 UTC), that are
/// image releases older than the [`KEEP`] newest.
pub fn stale(listed: &[(String, String)], keep: usize) -> Vec<String> {
    let mut images: Vec<&(String, String)> =
        listed.iter().filter(|(tag, _)| tag.starts_with(TAG_PREFIX)).collect();
    images.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
    images.into_iter().skip(keep).map(|(tag, _)| tag.clone()).collect()
}

/// Write every asset of `tag`'s release into `out`: the image at `image`
/// compressed, its sum, the notes and the licence notice at `licences`.
/// Answers the paths in upload order.
pub fn write_assets(
    root: &Path,
    image: &Path,
    licences: &Path,
    out: &Path,
    tag: &str,
    commit: &str,
) -> Result<Vec<PathBuf>, String> {
    use flate2::write::GzEncoder;
    use sha2::{Digest, Sha256};

    let compressed = out.join(IMAGE_ASSET);
    let file = fs::File::create(&compressed).map_err(|e| format!("{}: {e}", compressed.display()))?;
    let mut gz = GzEncoder::new(file, flate2::Compression::default());
    let mut raw = fs::File::open(image).map_err(|e| format!("{}: {e}", image.display()))?;
    std::io::copy(&mut raw, &mut gz).map_err(|e| format!("compressing {}: {e}", image.display()))?;
    gz.finish().map_err(|e| format!("compressing {}: {e}", image.display()))?.flush().map_err(|e| e.to_string())?;

    let bytes = fs::read(&compressed).map_err(|e| format!("{}: {e}", compressed.display()))?;
    let sum: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    let sums = out.join(SUMS_ASSET);
    fs::write(&sums, format!("{sum}  {IMAGE_ASSET}\n")).map_err(|e| e.to_string())?;

    let readme = out.join(NOTES_ASSET);
    fs::write(&readme, notes(root, tag, commit)?).map_err(|e| e.to_string())?;

    let notice = out.join(LICENCES_ASSET);
    fs::copy(licences, &notice).map_err(|e| format!("{}: {e}", licences.display()))?;
    Ok(vec![compressed, sums, readme, notice])
}

/// `gh <args>`: what it printed, or its exit and what it said.
fn gh(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("gh").args(args).current_dir(root).output().map_err(|e| format!("gh: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("gh {} exited {}: {}", args.join(" "), out.status, String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// A release GitHub holds under a tag: whether it is still a draft, and
/// whether it carries [`IMAGE_ASSET`].
#[derive(Debug, PartialEq, Eq)]
struct Held {
    draft: bool,
    image: bool,
}

/// What GitHub holds under `tag`: nothing, which `gh` says as exactly
/// `release not found`, or a [`Held`]. Any other failure of `gh` is one.
fn held(root: &Path, tag: &str) -> Result<Option<Held>, String> {
    let json = match gh(root, &["release", "view", tag, "--json", "isDraft,assets"]) {
        Err(why) if why.ends_with(": release not found") => return Ok(None),
        said => said?,
    };
    let v: Value = serde_json::from_str(&json).map_err(|e| format!("gh release view {tag}: {e}"))?;
    let draft = v["isDraft"].as_bool().ok_or_else(|| format!("gh release view {tag} gave no isDraft: {json}"))?;
    let assets = v["assets"].as_array().ok_or_else(|| format!("gh release view {tag} gave no assets: {json}"))?;
    let image = assets.iter().any(|a| a["name"].as_str() == Some(IMAGE_ASSET));
    Ok(Some(Held { draft, image }))
}

/// Every release, as (tag, creation time), refusing a listing that filled
/// [`LISTED`] or an entry without both.
fn listed(root: &Path) -> Result<Vec<(String, String)>, String> {
    let limit = LISTED.to_string();
    let json = gh(root, &["release", "list", "--limit", &limit, "--json", "tagName,createdAt"])?;
    let v: Value = serde_json::from_str(&json).map_err(|e| format!("gh release list: {e}"))?;
    let all = v.as_array().ok_or_else(|| format!("gh release list gave no list: {json}"))?;
    if all.len() >= LISTED {
        return Err(format!("gh release list gave {} releases, its limit, so some are not in it", all.len()));
    }
    all.iter()
        .map(|r| match (r["tagName"].as_str(), r["createdAt"].as_str()) {
            (Some(tag), Some(at)) => Ok((tag.to_string(), at.to_string())),
            _ => Err(format!("gh release list gave a release without a tag and a time: {r}")),
        })
        .collect()
}

/// Upload `assets` as a draft of `tag`, and publish it once GitHub holds the
/// image, so a failed upload leaves nothing public.
fn publish(root: &Path, tag: &str, commit: &str, notes: &Path, assets: &[PathBuf]) -> Result<(), String> {
    let notes = notes.display().to_string();
    let mut args: Vec<&str> =
        vec!["release", "create", tag, "--draft", "--title", tag, "--target", commit, "--notes-file", &notes];
    let assets: Vec<String> = assets.iter().map(|a| a.display().to_string()).collect();
    args.extend(assets.iter().map(String::as_str));
    gh(root, &args)?;
    let drafted = held(root, tag)?;
    if drafted != Some(Held { draft: true, image: true }) {
        return Err(format!("{tag} was created as a draft carrying {IMAGE_ASSET}, and GitHub holds {drafted:?}"));
    }
    gh(root, &["release", "edit", tag, "--draft=false", "--latest"])?;
    let published = held(root, tag)?;
    if published != Some(Held { draft: false, image: true }) {
        return Err(format!("{tag} was published, and GitHub holds {published:?}"));
    }
    Ok(())
}

/// `cargo run -- --ci release`: build the release image once, boot a copy of
/// it under this host's line, and on `main` publish those bytes unless this
/// commit's are, then delete the image releases past [`KEEP`].
pub fn release(root: &Path) -> Result<String, String> {
    if !std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true") {
        return Err("only a runner releases an image".into());
    }
    let commit = std::env::var("GITHUB_SHA").map_err(|_| "GITHUB_SHA is unset".to_string())?;
    let head = crate::pr::git(root, &["rev-parse", "HEAD"])?;
    if head != commit {
        return Err(format!("the checkout is {head} and the run is of {commit}"));
    }
    let tag = tag(&commit)?;
    let on_main = std::env::var("GITHUB_REF").ok().as_deref() == Some("refs/heads/main");
    let host = Host::this()?;

    let mut said = match held(root, &tag)? {
        Some(Held { draft: false, image: true }) => format!("{tag} is already published"),
        Some(other) => return Err(format!("GitHub holds {tag} as {other:?}: a failed run's, to delete by hand")),
        None => {
            let boot = crate::build::Boot::release(root);
            let plan = crate::build::plan_for(root, &boot, false, &[]);
            let image = crate::build::build(root, boot, false, &plan);
            let out = toyos_tmpdir::TempDir::new("image-release");
            let stick = out.join("stick.img");
            fs::copy(&image, &stick).map_err(|e| format!("copy {} to {}: {e}", image.display(), stick.display()))?;
            boots(root, host, &stick, DESKTOP, &out.join("qemu.stderr"))?;
            fs::remove_file(&stick).map_err(|e| format!("{}: {e}", stick.display()))?;
            if !on_main {
                return Ok(format!("{tag} booted to its desktop under {host:?}'s line; off main, so not published"));
            }
            let licences = root.join(crate::build::RELEASE_NOTICES);
            let assets = write_assets(root, &image, &licences, &out, &tag, &commit)?;
            publish(root, &tag, &commit, &out.join(NOTES_ASSET), &assets)?;
            format!("{tag} booted to its desktop under {host:?}'s line and is published")
        }
    };
    if !on_main {
        return Ok(said);
    }
    let old = stale(&listed(root)?, KEEP);
    for old in &old {
        gh(root, &["release", "delete", old, "--cleanup-tag", "--yes"])?;
    }
    said.push_str(&format!("; {} older image release(s) deleted, {KEEP} kept", old.len()));
    Ok(said)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn notes_of_a_commit() -> String {
        notes(&root(), "image-x86_64-c55189490123", &"c".repeat(40)).unwrap()
    }

    #[test]
    fn a_tag_is_the_commit_and_a_short_or_foreign_id_is_refused() {
        let commit = "c55189490123456789abcdef0123456789abcdef";
        assert_eq!(tag(commit).unwrap(), "image-x86_64-c55189490123");
        assert!(tag("c5518949").is_err());
        assert!(tag(&commit.to_uppercase()).is_err());
    }

    /// Only image releases are judged, the newest are kept by creation time,
    /// and the toolchain's releases are never named.
    #[test]
    fn retention_deletes_only_image_releases_past_the_newest_kept() {
        let at = |day: u32| format!("2026-09-{day:02}T03:00:00Z");
        let mut listed: Vec<(String, String)> =
            (1..=10).map(|day| (format!("{TAG_PREFIX}{day:012}"), at(day))).collect();
        listed.push(("toolchain-linux-x86_64-0123456789abcdef".into(), at(1)));
        let old = stale(&listed, 7);
        assert_eq!(old, [3, 2, 1].map(|day| format!("{TAG_PREFIX}{day:012}")));
        assert!(stale(&listed[..7], 7).is_empty());
        assert_eq!(stale(&listed, 0).len(), 10);
    }

    /// Each host's command boots the image it is given as the one stick, on
    /// that host's own accelerator and firmware, and the notes print it whole.
    #[test]
    fn the_notes_print_each_hosts_command_line_whole() {
        let notes = notes_of_a_commit();
        for host in Host::ALL {
            let argv = host.command(IMAGE);
            assert!(argv.iter().any(|a| a == &format!("if=none,id=stick,format=raw,file={IMAGE}")));
            assert!(argv.iter().any(|a| a == host.accel().name()));
            for firmware in host.firmware() {
                assert!(argv.iter().any(|a| a.contains(firmware)), "{host:?}: {firmware}");
            }
            let printed = shell(&argv);
            assert!(notes.contains(&printed), "{host:?}'s command is not in the notes");
            let words: Vec<&str> = printed.split_whitespace().filter(|w| *w != "\\").collect();
            assert_eq!(words, argv.iter().map(String::as_str).collect::<Vec<_>>());
        }
    }

    /// Before the line that writes the stick, the notes name the firmware
    /// variable a boot leaves behind by the name and vendor the loader writes
    /// it under, how it is removed, and `BootNext`; and they name everything
    /// the release leaves out.
    #[test]
    fn the_notes_name_what_a_boot_writes_to_the_firmware_before_the_stick_is_written() {
        let notes = notes_of_a_commit();
        let (floor, hex) = floor_variable();
        let dd = notes.find("    dd if=").expect("no dd line");
        let named = format!("`{floor}` and {hex} hex digits");
        let vendor = format!("`{}`", toyos_update::floor::VENDOR);
        for said in [named.as_str(), &vendor, "dmpstore -d", "`BootNext`", "the next restart boots the stick once", "unmountDisk"] {
            let at = notes.find(said).unwrap_or_else(|| panic!("the notes never say {said:?}"));
            assert!(at < dd, "{said:?} comes after the dd line");
        }
        for withheld in pending_owner() {
            let (Subject::Crate(path) | Subject::Notice(path) | Subject::File(path)) = withheld;
            assert!(notes.contains(&format!("`{path}`")), "the notes do not say {path} is left out");
        }
    }

    /// What `sha256sum -c` checks is the compressed asset's own digest, the
    /// asset decompresses to the image byte for byte, and the notice is
    /// carried as it was written.
    #[test]
    fn the_sum_is_the_compressed_images_and_it_decompresses_to_the_image() {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let dir = toyos_tmpdir::TempDir::new("image-assets");
        let image = dir.join("in.img");
        let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).chain(std::iter::repeat_n(0, 1 << 20)).collect();
        fs::write(&image, &bytes).unwrap();
        let licences = dir.join("notice.txt");
        fs::write(&licences, "the notice").unwrap();
        let out = dir.join("out");
        fs::create_dir(&out).unwrap();
        let assets = write_assets(&root(), &image, &licences, &out, "image-x86_64-c55189490123", &"c".repeat(40)).unwrap();
        let names: Vec<String> =
            assets.iter().map(|a| a.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, [IMAGE_ASSET, SUMS_ASSET, NOTES_ASSET, LICENCES_ASSET]);
        assert_eq!(fs::read_to_string(out.join(LICENCES_ASSET)).unwrap(), "the notice");

        let gz = fs::read(out.join(IMAGE_ASSET)).unwrap();
        let sum: String = Sha256::digest(&gz).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(fs::read_to_string(out.join(SUMS_ASSET)).unwrap(), format!("{sum}  {IMAGE_ASSET}\n"));
        let mut back = Vec::new();
        flate2::read::GzDecoder::new(&gz[..]).read_to_end(&mut back).unwrap();
        assert!(back == bytes, "the asset does not decompress to the image");
    }
}
