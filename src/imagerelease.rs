//! The image release: the disk a person downloads and boots under QEMU or
//! writes to a stick, published by `cargo run -- --ci release` from a commit of
//! `main` whose whole nightly was green.
//!
//! **Named by that commit**, [`tag`]: every build draws its partition GUIDs and
//! a runner mints its own throwaway signing key (`src/signing.rs`), so the
//! bytes name nothing a second build reproduces and a content hash would name
//! one build. **Kept to [`KEEP`]**: each publish deletes every older image
//! release and its tag ([`stale`]).
//!
//! The image is `build::Boot::release`'s. The notes carry the two command lines
//! [`Host::command`] declares, which are the argv `release_command_boots` and
//! `release_writes_no_other_disk` boot.
//!
//! Not in `src/release.rs`, whose bytes are hashed into the toolchain's tag.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::arch::{Accel, Arch};

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

/// Every licence text the release carries beside the image, as (the file in
/// this tree, its asset name): ToyOS's own two, the ledger, and each text
/// `NOTICE` names for the third-party files the release image ships
/// (`every_text_notice_names_for_what_the_release_ships_is_carried`).
pub const LICENCE_ASSETS: &[(&str, &str)] = &[
    ("LICENSE-MIT", "LICENSE-MIT"),
    ("LICENSE-APACHE", "LICENSE-APACHE"),
    ("NOTICE", "NOTICE"),
    ("licenses/OFL-1.1-JetBrainsMono.txt", "OFL-1.1-JetBrainsMono.txt"),
    ("licenses/MIT-PhosphorIcons.txt", "MIT-PhosphorIcons.txt"),
    ("assets/fonts/OFL.txt", "OFL-1.1-OpenSans.txt"),
];

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

    /// The one of the two this machine is, or why it is neither.
    pub fn this() -> Result<Host, String> {
        if cfg!(target_os = "macos") && Arch::HOST == Some(Arch::Aarch64) {
            return Ok(Host::MacosAppleSilicon);
        }
        if cfg!(target_os = "linux") && Arch::HOST == Some(Arch::X86_64) && Arch::X86_64.accel() == Accel::Kvm {
            return Ok(Host::LinuxKvm);
        }
        Err("this host is neither an Apple Silicon Mac nor an x86-64 Linux whose /dev/kvm opens, \
             so no command line the release notes print is this host's"
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

/// The release notes, which are also [`NOTES_ASSET`].
pub fn notes(root: &Path, tag: &str, commit: &str) -> Result<String, String> {
    let qemu = crate::ci::declared_qemu_version(root).ok_or(".github/qemu-version declares no version")?;
    let data = toyos_gpt::Guid::TOYOS_DATA_TEXT;
    let mut commands = String::new();
    for host in Host::ALL {
        commands.push_str(&format!("{}:\n\n{}\n\n", host.named(), shell(&host.command(IMAGE))));
    }
    let texts: Vec<String> = LICENCE_ASSETS.iter().map(|(_, asset)| format!("`{asset}`")).collect();
    Ok(format!(
        "# ToyOS {tag}

The ToyOS disk image of commit {commit} on `main`, whose nightly was green. It boots under QEMU, or from a USB stick on a UEFI x86-64 machine.

## Verify and unpack

Download `{IMAGE_ASSET}` and `{SUMS_ASSET}` from this release into one directory, and in it:

    sha256sum -c {SUMS_ASSET}
    gunzip {IMAGE_ASSET}

## Under QEMU

QEMU {qemu} is the version ToyOS is measured with. The firmware is edk2, from Homebrew's QEMU on macOS and from Debian's `ovmf` on Linux.

{commands}The kernel's log is on the terminal and the desktop is in QEMU's window. The running system writes to `{IMAGE}` itself, as it would to a stick. `/apps`, `/config`, `/home` and `/state` are kept in memory and are gone at the next boot.

## On a USB stick

The machine has to be an x86-64 PC from 2020 or later, booting UEFI with Secure Boot off. The only hardware ToyOS is known to work on is a Lenovo ThinkPad T14; on anything else it is untried.

Write `{IMAGE}` to the whole stick, not to a partition of it, as root; what the stick held is lost:

    dd if={IMAGE} of=/dev/<the stick> bs=4194304
    sync

What a boot writes on the machine:

- the stick it booted from;
- the firmware's variable store: the loader's anti-rollback floor, and `BootNext` where a boot entry names the stick;
- another disk only where it carries a partition of ToyOS's DATA type, `{data}`, a type no other system uses. That is where `/apps`, `/config`, `/home` and `/state` live. Every other disk is read for its partition table and never written: `release_writes_no_other_disk` boots this image beside a disk laid out as another operating system's and compares every byte of it.

## Terms

ToyOS is MIT OR Apache-2.0. `NOTICE` names every third-party file in the repository and its terms; this release carries it and the texts for what the image ships: {texts}.

The image leaves out doom, `DOOM1.WAD` and the SoundFont: whether they may ship is the owner's to rule (`src/licence.rs`), and nothing is published while it is not.
",
        texts = texts.join(", "),
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
/// compressed, its sum, the notes and the licence texts. Answers the paths in
/// upload order.
pub fn write_assets(root: &Path, image: &Path, out: &Path, tag: &str, commit: &str) -> Result<Vec<PathBuf>, String> {
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

    let mut assets = vec![compressed, sums, readme];
    for (from, name) in LICENCE_ASSETS {
        let to = out.join(name);
        fs::copy(root.join(from), &to).map_err(|e| format!("{from}: {e}"))?;
        assets.push(to);
    }
    Ok(assets)
}

fn gh(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("gh").args(args).current_dir(root).output().map_err(|e| format!("gh: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("gh {} exited {}: {}", args.join(" "), out.status, String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Whether `tag` is a release carrying [`IMAGE_ASSET`].
fn published(root: &Path, tag: &str) -> bool {
    gh(root, &["release", "view", tag, "--json", "assets", "--jq", ".assets[].name"])
        .is_ok_and(|names| names.lines().any(|name| name == IMAGE_ASSET))
}

/// `cargo run -- --ci release`: publish this commit's image unless it is
/// published, then delete the image releases past [`KEEP`]. Only a nightly on
/// `main` publishes, and `nightly.yml` runs this only once every lane of that
/// nightly is green.
pub fn publish(root: &Path) -> Result<String, String> {
    let on_runner = std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true");
    if !on_runner || std::env::var("GITHUB_REF").ok().as_deref() != Some("refs/heads/main") {
        return Err("only a nightly on main publishes an image".into());
    }
    let commit = std::env::var("GITHUB_SHA").map_err(|_| "GITHUB_SHA is unset".to_string())?;
    let head = crate::pr::git(root, &["rev-parse", "HEAD"])?;
    if head != commit {
        return Err(format!("the checkout is {head} and the nightly ran {commit}"));
    }
    let tag = tag(&commit)?;

    let mut said = if published(root, &tag) {
        format!("{tag} is already published")
    } else {
        let built = Command::new("cargo")
            .args(["run", "--", "--release-boot", "--build-only"])
            .current_dir(root)
            .status()
            .map_err(|e| format!("cargo: {e}"))?;
        if !built.success() {
            return Err(format!("cargo run -- --release-boot --build-only exited {built}"));
        }
        let out = toyos_tmpdir::TempDir::new("image-release");
        let assets = write_assets(root, &root.join(crate::build::RELEASE_IMAGE), &out, &tag, &commit)?;
        let notes = out.join(NOTES_ASSET);
        let mut args: Vec<String> = ["release", "create", &tag, "--title", &tag, "--target", &commit, "--latest", "--notes-file"]
            .map(String::from)
            .to_vec();
        args.push(notes.display().to_string());
        args.extend(assets.iter().map(|a| a.display().to_string()));
        gh(root, &args.iter().map(String::as_str).collect::<Vec<_>>())?;
        if !published(root, &tag) {
            return Err(format!("{tag} was created and carries no {IMAGE_ASSET}"));
        }
        format!("{tag} published")
    };

    let listed = gh(root, &["release", "list", "--limit", "1000", "--json", "tagName,createdAt", "--jq", ".[] | .tagName + \" \" + .createdAt"])?;
    let listed: Vec<(String, String)> = listed
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(t, c)| (t.to_string(), c.to_string()))
        .collect();
    let old = stale(&listed, KEEP);
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
        let notes = notes(&root(), "image-x86_64-c55189490123", &"c".repeat(40)).unwrap();
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

    /// The release carries the text `NOTICE` names for every third-party file
    /// under an asset directory the release image ships, and none only
    /// withheld material names.
    #[test]
    fn every_text_notice_names_for_what_the_release_ships_is_carried() {
        use crate::licence::Subject;
        let root = root();
        let notice = fs::read_to_string(root.join("NOTICE")).unwrap();
        let assets: Vec<String> = crate::build::shipped(&root)
            .unwrap()
            .assets
            .iter()
            .map(|dir| format!("{}/", dir.strip_prefix(&root).unwrap().display()))
            .collect();
        let withheld: Vec<&str> = crate::licence::pending_owner()
            .map(|s| match s {
                Subject::Crate(p) | Subject::Notice(p) | Subject::File(p) => p,
            })
            .collect();
        let carried: Vec<&str> = LICENCE_ASSETS.iter().map(|(from, _)| *from).collect();
        let mut needed = vec!["LICENSE-MIT", "LICENSE-APACHE", "NOTICE"];
        let mut shipped_sections = 0;
        let sections = crate::licence::sections(&notice);
        for section in &sections {
            let ships = assets.iter().any(|dir| section.path.starts_with(dir.as_str()))
                && !withheld.contains(&section.path.as_str());
            if !ships {
                continue;
            }
            shipped_sections += 1;
            for text in &section.texts {
                assert!(carried.contains(&text.as_str()), "{} names {text}, which the release does not carry", section.path);
                needed.push(text.as_str());
            }
        }
        assert!(shipped_sections >= 3, "{shipped_sections} NOTICE sections read as shipped");
        for from in &carried {
            assert!(root.join(from).is_file(), "{from}");
            assert!(needed.contains(from), "{from} is carried and nothing the release ships names it");
        }
        let names: Vec<&str> = LICENCE_ASSETS.iter().map(|(_, name)| *name).collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "two texts under one asset name: {names:?}");
    }

    /// What `sha256sum -c` checks is the compressed asset's own digest, and the
    /// asset decompresses to the image byte for byte.
    #[test]
    fn the_sum_is_the_compressed_images_and_it_decompresses_to_the_image() {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        let dir = toyos_tmpdir::TempDir::new("image-assets");
        let image = dir.join("in.img");
        let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).chain(std::iter::repeat_n(0, 1 << 20)).collect();
        fs::write(&image, &bytes).unwrap();
        let out = dir.join("out");
        fs::create_dir(&out).unwrap();
        let assets = write_assets(&root(), &image, &out, "image-x86_64-c55189490123", &"c".repeat(40)).unwrap();
        let names: Vec<String> =
            assets.iter().map(|a| a.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names[..3], [IMAGE_ASSET, SUMS_ASSET, NOTES_ASSET]);
        assert_eq!(names.len(), 3 + LICENCE_ASSETS.len());

        let gz = fs::read(out.join(IMAGE_ASSET)).unwrap();
        let sum: String = Sha256::digest(&gz).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(fs::read_to_string(out.join(SUMS_ASSET)).unwrap(), format!("{sum}  {IMAGE_ASSET}\n"));
        let mut back = Vec::new();
        flate2::read::GzDecoder::new(&gz[..]).read_to_end(&mut back).unwrap();
        assert!(back == bytes, "the asset does not decompress to the image");
    }
}
