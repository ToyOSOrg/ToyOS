//! The outside judge for `bcachefs/`'s upstream read path.
//!
//! **The oracle is upstream's own implementation, not a second reading of its
//! source.** A Linux guest boots the pinned Arch Linux install medium, which
//! carries `bcachefs-tools` at the release the crate's format citation names;
//! that tool formats a disk, mounts it through libbcachefs, writes a tree into
//! it, states each file's SHA-256, unmounts and runs `fsck`. The host then
//! reads the same disk with ToyOS's reader and has to agree with every one of
//! those sentences.
//!
//! Nothing outside Rust and QEMU runs: the image is fetched by
//! `toyos_build::oracle` and the guest is driven over its serial console.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bcachefs::upstream::fs::{FileKind, Volume};

use super::lane;

/// The volume the guest writes, and how big it is.
const SCRATCH_MB: u64 = 1024;
/// The deterministic file: `seq 1 40000`, which the host can predict exactly
/// and which is long enough to need more than one extent.
const SEQ_LINES: u32 = 40_000;
const HELLO: &str = "hello-toyos";
const LINK_TARGET: &str = "../a.txt";

/// A guest whose console the test reads and types at.
struct Guest {
    child: Child,
    log: Arc<Mutex<String>>,
    started: Instant,
}

impl Guest {
    fn boot(iso: &Path, scratch: &Path) -> Result<Self, String> {
        let mut child = Command::new("qemu-system-x86_64")
            .args(["-m", "4096", "-smp", "4"])
            .args(["-cdrom", &iso.display().to_string()])
            .arg("-drive")
            .arg(format!("file={},format=raw,if=virtio", scratch.display()))
            .args(["-display", "none", "-serial", "stdio", "-monitor", "none", "-no-reboot"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("launching the oracle guest: {e}"))?;

        let log = Arc::new(Mutex::new(String::new()));
        let mut out = child.stdout.take().expect("the guest's console is piped");
        let sink = Arc::clone(&log);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(read) = out.read(&mut buf) {
                if read == 0 {
                    break;
                }
                sink.lock()
                    .expect("the console log's lock")
                    .push_str(&String::from_utf8_lossy(&buf[..read]));
            }
        });
        Ok(Self { child, log, started: Instant::now() })
    }

    fn console(&self) -> String {
        self.log.lock().expect("the console log's lock").clone()
    }

    /// Block until `marker` appears, or say how long was spent not seeing it.
    ///
    /// The guest is polled rather than timed: the boot's wall clock moves with
    /// the host, and every wait here is for a sentence the guest prints.
    fn wait_for(&mut self, marker: &str, budget: Duration) -> Result<(), String> {
        let until = Instant::now() + budget;
        loop {
            if self.console().contains(marker) {
                return Ok(());
            }
            if let Some(status) = self.child.try_wait().map_err(|e| format!("{e}"))? {
                return Err(format!(
                    "the oracle guest exited {status} at +{}s without printing {marker:?}",
                    self.started.elapsed().as_secs()
                ));
            }
            if Instant::now() >= until {
                let log = self.console();
                let tail: String = log.chars().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect();
                return Err(format!(
                    "the oracle guest did not print {marker:?} within {}s. Console tail:\n{tail}",
                    budget.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn send(&mut self, text: &str) -> Result<(), String> {
        let stdin = self.child.stdin.as_mut().expect("the guest's console is piped");
        stdin.write_all(text.as_bytes()).map_err(|e| format!("typing at the guest: {e}"))?;
        stdin.flush().map_err(|e| format!("typing at the guest: {e}"))
    }

    /// Run one shell line and block until it has finished.
    ///
    /// The done-marker is assembled by the shell so the line the tty echoes
    /// back never contains it — otherwise every wait would return on the echo
    /// of its own command.
    fn run(&mut self, tag: &str, command: &str, budget: Duration) -> Result<(), String> {
        self.send(&format!("{command}; echo \"DO\"\"NE_{tag} rc=$?\"\n"))?;
        self.wait_for(&format!("DONE_{tag} rc="), budget)
    }
}

impl Drop for Guest {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What the guest said about one file.
fn stated_sha(console: &str, path: &str) -> Result<String, String> {
    console
        .lines()
        .find_map(|line| {
            let (sum, name) = line.split_once("  ")?;
            (name.trim_end() == path && sum.len() == 64 && sum.chars().all(|c| c.is_ascii_hexdigit()))
                .then(|| sum.to_string())
        })
        .ok_or_else(|| format!("the guest never stated a SHA-256 for {path}"))
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// `seq 1 40000`'s output, which the guest writes and the host predicts.
fn seq_text() -> Vec<u8> {
    let mut out = Vec::new();
    for n in 1..=SEQ_LINES {
        out.extend_from_slice(n.to_string().as_bytes());
        out.push(b'\n');
    }
    out
}

/// A whole raw image as 4096-byte blocks.
struct WholeFile {
    file: std::cell::RefCell<std::fs::File>,
    blocks: u64,
}

struct HostIoFailed;
impl bcachefs::TransferError for HostIoFailed {
    fn refused_before_attempt(&self) -> bool {
        false
    }
}

impl bcachefs::BlockIO for WholeFile {
    fn read_block(
        &self,
        block: bcachefs::BlockNum,
        buf: &mut bcachefs::BlockBuf,
    ) -> Result<(), bcachefs::DeviceError> {
        use std::io::{Seek, SeekFrom};
        let mut file = self.file.borrow_mut();
        file.seek(SeekFrom::Start(block.raw() * 4096))
            .map_err(|_| bcachefs::DeviceError::classify(&HostIoFailed))?;
        file.read_exact(buf.as_bytes_mut())
            .map_err(|_| bcachefs::DeviceError::classify(&HostIoFailed))
    }

    fn write_block(
        &self,
        _block: bcachefs::BlockNum,
        _buf: &bcachefs::BlockBuf,
    ) -> Result<(), bcachefs::DeviceError> {
        Err(bcachefs::DeviceError::classify(&HostIoFailed))
    }

    fn block_count(&self) -> u64 {
        self.blocks
    }
}

fn open_whole(path: &Path) -> Result<WholeFile, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let blocks = file.metadata().map_err(|e| format!("{e}"))?.len() / 4096;
    Ok(WholeFile { file: std::cell::RefCell::new(file), blocks })
}

/// The whole judgement: upstream writes a volume, ToyOS reads it back.
pub fn bcachefs_upstream_read() -> Result<(), String> {
    let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target");
    let iso = toyos_build::oracle::fetch(&toyos_build::oracle::ARCH_ISO, &target)?;

    let scratch = lane::dir().join("bcachefs-oracle.img");
    let file = std::fs::File::create(&scratch)
        .map_err(|e| format!("creating {}: {e}", scratch.display()))?;
    file.set_len(SCRATCH_MB * 1024 * 1024).map_err(|e| format!("sizing the scratch disk: {e}"))?;
    drop(file);

    let mut guest = Guest::boot(&iso, &scratch)?;

    // The install medium's syslinux mirrors its menu to the serial line and
    // takes input there; Tab opens the highlighted entry's command line, which
    // is the only way to ask this image for a serial console.
    guest.wait_for("Press [Tab] to edit options", Duration::from_secs(300))?;
    std::thread::sleep(Duration::from_secs(3));
    guest.send("\t")?;
    std::thread::sleep(Duration::from_secs(3));
    guest.send(" console=ttyS0,115200\n")?;

    guest.wait_for("archiso login:", Duration::from_secs(1200))?;
    std::thread::sleep(Duration::from_secs(2));
    guest.send("root\n")?;
    std::thread::sleep(Duration::from_secs(5));
    guest.run("LOGIN", "true", Duration::from_secs(300))?;

    guest.run(
        "SRC",
        &format!(
            "rm -rf /tmp/src; mkdir -p /tmp/src/Documents/deep /tmp/src/empty; \
             printf {HELLO} > /tmp/src/Documents/a.txt; \
             seq 1 {SEQ_LINES} > /tmp/src/Documents/deep/seq.txt; \
             head -c 300000 /dev/urandom > /tmp/src/Documents/deep/big.bin; \
             ln -s {LINK_TARGET} /tmp/src/Documents/deep/link"
        ),
        Duration::from_secs(300),
    )?;
    guest.run("FORMAT", "bcachefs format --fs_label=toyosjudge /dev/vda", Duration::from_secs(600))?;
    guest.run(
        "MOUNT",
        "mkdir -p /mnt/bch; bcachefs fusemount /dev/vda /mnt/bch >/tmp/fuse.log 2>&1 & sleep 10; mount | grep -c fuse.bcachefs",
        Duration::from_secs(300),
    )?;
    guest.run("COPY", "cp -a /tmp/src/. /mnt/bch/ && sync", Duration::from_secs(600))?;
    guest.run(
        "STATE",
        "sha256sum /mnt/bch/Documents/a.txt /mnt/bch/Documents/deep/seq.txt /mnt/bch/Documents/deep/big.bin",
        Duration::from_secs(300),
    )?;
    guest.run("UMOUNT", "fusermount3 -u /mnt/bch; sleep 3; sync", Duration::from_secs(300))?;
    guest.run("FSCK", "bcachefs fsck -y /dev/vda", Duration::from_secs(900))?;
    guest.send("poweroff -f\n")?;
    std::thread::sleep(Duration::from_secs(5));

    let console = guest.console();
    drop(guest);

    // Upstream's own checker has to have passed before its volume is used as
    // ground truth for anything.
    for pass in ["check_inodes", "check_extents", "check_dirents", "check_directory_structure"] {
        if !console.contains(&format!("{pass}... done")) {
            return Err(format!("upstream's fsck did not report {pass} clean:\n{console}"));
        }
    }
    if !console.contains("DONE_FSCK rc=0") {
        return Err(format!("upstream's fsck did not exit 0:\n{console}"));
    }

    let stated: Vec<(&str, String)> = ["a.txt", "seq.txt", "big.bin"]
        .iter()
        .map(|name| {
            let path = if *name == "a.txt" {
                "/mnt/bch/Documents/a.txt".to_string()
            } else {
                format!("/mnt/bch/Documents/deep/{name}")
            };
            stated_sha(&console, &path).map(|sum| (*name, sum))
        })
        .collect::<Result<_, _>>()?;

    let volume = Volume::open(open_whole(&scratch)?)
        .map_err(|e| format!("ToyOS could not open the volume upstream wrote: {e:?}\n{console}"))?;

    let read = |path: &str| -> Result<Vec<u8>, String> {
        let (inum, kind) = volume
            .resolve(path)
            .map_err(|e| format!("resolving {path}: {e:?}"))?
            .ok_or_else(|| format!("ToyOS's reader found no {path} in the volume upstream wrote"))?;
        if kind != FileKind::Regular {
            return Err(format!("{path} read back as {kind:?} rather than a file"));
        }
        volume.read(inum).map_err(|e| format!("reading {path}: {e:?}"))
    };

    for (name, want) in &stated {
        let path = if *name == "a.txt" {
            "/Documents/a.txt".to_string()
        } else {
            format!("/Documents/deep/{name}")
        };
        let got = read(&path)?;
        let sum = sha256_hex(&got);
        if sum != *want {
            return Err(format!(
                "{path}: upstream wrote SHA-256 {want}, ToyOS read back {sum} ({} bytes)",
                got.len()
            ));
        }
    }

    // Two of the three are predictable without the guest, so a harness that
    // misread the guest's own statement still cannot pass.
    if read("/Documents/a.txt")? != HELLO.as_bytes() {
        return Err("the inline-data file did not read back as what it was written as".to_string());
    }
    if read("/Documents/deep/seq.txt")? != seq_text() {
        return Err("the multi-extent file did not read back as the host's own copy of it".to_string());
    }

    let (link, kind) = volume
        .resolve("/Documents/deep/link")
        .map_err(|e| format!("resolving the symlink: {e:?}"))?
        .ok_or("ToyOS's reader found no symlink in the volume upstream wrote")?;
    if kind != FileKind::Symlink {
        return Err(format!("the symlink read back as {kind:?}"));
    }
    let target = volume.read_link(link).map_err(|e| format!("reading the symlink: {e:?}"))?;
    if target != LINK_TARGET {
        return Err(format!("the symlink points at {target:?}, not {LINK_TARGET:?}"));
    }

    // Nesting: `/Documents/deep` is two directories down, and `/empty` has to
    // read back as a directory with nothing in it rather than as absent.
    let (empty, kind) = volume
        .resolve("/empty")
        .map_err(|e| format!("resolving /empty: {e:?}"))?
        .ok_or("ToyOS's reader lost the empty directory")?;
    if kind != FileKind::Dir {
        return Err(format!("/empty read back as {kind:?}"));
    }
    let mut entries = 0usize;
    volume
        .readdir(empty, &mut |_, _, _| {
            entries += 1;
            true
        })
        .map_err(|e| format!("listing /empty: {e:?}"))?;
    if entries != 0 {
        return Err(format!("/empty listed {entries} entries"));
    }

    let mut names = Vec::new();
    volume
        .readdir(volume.root(), &mut |name, _, _| {
            names.push(name.to_string());
            true
        })
        .map_err(|e| format!("listing the root: {e:?}"))?;
    names.sort();
    let want = ["Documents", "empty", "lost+found"];
    if names != want {
        return Err(format!("the root listed {names:?}, and upstream put {want:?} there"));
    }
    Ok(())
}
