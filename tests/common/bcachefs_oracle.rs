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
use std::collections::BTreeSet;
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
/// The prefix the guest puts on each line of its own listing, so the host can
/// pick the listing out of a console that carries a whole distribution boot.
const TREE_MARK: &str = "TREEENTRY";
/// What follows an exit status, so a chunk that ends inside one is waited out
/// rather than read short.
const RC_END: &str = ".";
const RC_WAIT: Duration = Duration::from_secs(60);

/// A guest whose console the test reads and types at.
struct Guest {
    child: Child,
    log: Arc<Mutex<String>>,
    started: Instant,
    /// How much of the console has been matched already. **Every wait is for
    /// something the guest has not said yet**: a marker matched against the
    /// whole accumulated console matches retroactively, so a step would fire on
    /// a line printed long before it and type into a guest that is not
    /// listening for it.
    cursor: usize,
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
        Ok(Self { child, log, started: Instant::now(), cursor: 0 })
    }

    fn console(&self) -> String {
        self.log.lock().expect("the console log's lock").clone()
    }

    /// What the guest has said since the last thing this test matched.
    fn unread(console: &str, cursor: usize) -> &str {
        console.get(cursor..).unwrap_or("")
    }

    fn tail(console: &str) -> String {
        console.chars().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect()
    }

    /// Block until `marker` appears in what the guest has not said yet, and
    /// consume through it.
    ///
    /// The guest is polled rather than timed: the boot's wall clock moves with
    /// the host, and every wait here is for a sentence the guest prints.
    fn wait_for(&mut self, marker: &str, budget: Duration) -> Result<(), String> {
        let until = Instant::now() + budget;
        loop {
            let console = self.console();
            if let Some(at) = Self::unread(&console, self.cursor).find(marker) {
                self.cursor += at + marker.len();
                return Ok(());
            }
            if let Some(status) = self.child.try_wait().map_err(|e| format!("{e}"))? {
                return Err(format!(
                    "the oracle guest exited {status} at +{}s without printing {marker:?}",
                    self.started.elapsed().as_secs()
                ));
            }
            if Instant::now() >= until {
                return Err(format!(
                    "the oracle guest did not print {marker:?} within {}s. Console tail:\n{}",
                    budget.as_secs(),
                    Self::tail(&console)
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

    /// Run one shell line, block until it has finished, and refuse a non-zero
    /// exit.
    ///
    /// **The exit code is the whole point of waiting**: a `bcachefs format`
    /// that failed prints its marker exactly like one that worked, and every
    /// assertion after it would then be about an empty disk. The done-marker is
    /// assembled by the shell so the line the tty echoes back never contains
    /// it — otherwise every wait would return on the echo of its own command.
    fn run(&mut self, tag: &str, command: &str, budget: Duration) -> Result<(), String> {
        self.send(&format!("{command}; echo \"DO\"\"NE_{tag} rc=$?{RC_END}\"\n"))?;
        self.wait_for(&format!("DONE_{tag} rc="), budget)?;

        // **The terminator is what makes the digits complete.** A serial chunk
        // can end between `rc=` and the code, and a snapshot taken then reads
        // an empty exit status off a guest that succeeded.
        let until = Instant::now() + RC_WAIT;
        loop {
            let console = self.console();
            let rest = Self::unread(&console, self.cursor);
            if let Some(end) = rest.find(RC_END) {
                let code = rest[..end].to_string();
                self.cursor += end + RC_END.len();
                if code == "0" {
                    return Ok(());
                }
                return Err(format!(
                    "the guest's {tag} exited {code:?}, not 0. Console tail:\n{}",
                    Self::tail(&console)
                ));
            }
            if Instant::now() >= until {
                return Err(format!(
                    "the guest's {tag} never finished stating its exit status. Console tail:\n{}",
                    Self::tail(&console)
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Wait for a marker, then type — so nothing is typed at a guest that has
    /// not asked for it yet.
    fn after(&mut self, marker: &str, budget: Duration, text: &str) -> Result<(), String> {
        self.wait_for(marker, budget)?;
        self.send(text)
    }

    /// Block until the guest is gone, so the disk it wrote is closed before the
    /// host reads it.
    fn wait_gone(&mut self, budget: Duration) -> Result<(), String> {
        let until = Instant::now() + budget;
        while Instant::now() < until {
            if self.child.try_wait().map_err(|e| format!("{e}"))?.is_some() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Err(format!("the oracle guest was still running {}s after poweroff", budget.as_secs()))
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
    // is the only way to ask this image for a serial console. The menu echoes
    // the line it is about to boot, so the edit is driven off that echo rather
    // than off a timer that could type into an already-expired menu.
    guest.after("Press [Tab] to edit options", Duration::from_secs(300), "\t")?;
    guest.after("vmlinuz-linux", Duration::from_secs(120), " console=ttyS0,115200\n")?;
    guest.after("archiso login:", Duration::from_secs(1200), "root\n")?;
    // The medium's own message of the day, printed once the login has taken and
    // before the first prompt — so nothing is typed at a guest still logging in.
    guest.wait_for("installation guide", Duration::from_secs(300))?;
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
    // `fusemount` forks into the background, so the mount is waited for by the
    // mount table saying it is there — bounded, and printing the mounter's own
    // log when it does not, because a `fusemount` that died would otherwise
    // surface as a host timeout with nothing said about why.
    guest.run(
        "MOUNT",
        "mkdir -p /mnt/bch; bcachefs fusemount /dev/vda /mnt/bch >/tmp/fuse.log 2>&1 & \
         for _ in $(seq 1 600); do mount | grep -q fuse.bcachefs && break; sleep 0.1; done; \
         mount | grep -q fuse.bcachefs || { echo 'fusemount never mounted:'; cat /tmp/fuse.log; false; }",
        Duration::from_secs(300),
    )?;
    guest.run("COPY", "cp -a /tmp/src/. /mnt/bch/ && sync", Duration::from_secs(600))?;
    guest.run(
        "STATE",
        "sha256sum /mnt/bch/Documents/a.txt /mnt/bch/Documents/deep/seq.txt /mnt/bch/Documents/deep/big.bin",
        Duration::from_secs(300),
    )?;
    // What upstream says is on the volume, in full: the host diffs its own
    // listing against this in both directions, so a file ToyOS invents and a
    // file ToyOS loses are both red. The mark is split by the shell so the
    // command line the tty echoes back is not itself a listing entry, and it is
    // split out of the constant the host matches, so a drift is a diff.
    let (mark_head, mark_tail) = TREE_MARK.split_at(TREE_MARK.len() / 2);
    guest.run(
        "TREE",
        &format!("find /mnt/bch -mindepth 1 -printf '{mark_head}''{mark_tail} %y %p\\n' | LC_ALL=C sort"),
        Duration::from_secs(300),
    )?;
    guest.run(
        "UMOUNT",
        "fusermount3 -u /mnt/bch; \
         for _ in $(seq 1 600); do mount | grep -q fuse.bcachefs || break; sleep 0.1; done; \
         mount | grep -q fuse.bcachefs && { echo 'the mount outlived its unmount'; false; }; sync",
        Duration::from_secs(300),
    )?;
    guest.run("FSCK", "bcachefs fsck -y /dev/vda", Duration::from_secs(900))?;
    guest.send("poweroff -f\n")?;
    guest.wait_gone(Duration::from_secs(300))?;

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

    let volume = Volume::open(super::storage::FileBlocks::whole(&scratch)?)
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

    // **The whole tree, diffed against what upstream said it wrote, in both
    // directions.** A listing compared to a constant this file carries proves
    // only that this file and the reader agree; the guest's own `find` is what
    // makes it a judgement.
    let stated_tree = guest_tree(&console)?;
    let read_tree = walk(&volume, volume.root(), "/mnt/bch")?;
    let missing: Vec<&String> = stated_tree.iter().filter(|e| !read_tree.contains(*e)).collect();
    let invented: Vec<&String> = read_tree.iter().filter(|e| !stated_tree.contains(*e)).collect();
    if !missing.is_empty() || !invented.is_empty() {
        return Err(format!(
            "the tree upstream wrote and the tree ToyOS read differ.\n  \
             upstream wrote and ToyOS did not read: {missing:?}\n  \
             ToyOS read and upstream did not write: {invented:?}"
        ));
    }
    if stated_tree.len() < 7 {
        return Err(format!("upstream stated only {} tree entries; it wrote more", stated_tree.len()));
    }
    Ok(())
}

/// Every line the guest's `find` printed, as `<kind> <path>`.
fn guest_tree(console: &str) -> Result<BTreeSet<String>, String> {
    let tree: BTreeSet<String> = console
        .lines()
        .filter_map(|line| line.split_once(TREE_MARK).map(|(_, rest)| rest.trim().to_string()))
        .filter(|entry| !entry.is_empty() && entry.contains(' '))
        .collect();
    if tree.is_empty() {
        return Err("the guest never listed the tree it wrote".to_string());
    }
    Ok(tree)
}

/// The same listing, taken from ToyOS's reader, in the guest's own words.
fn walk<IO: bcachefs::BlockIO>(
    volume: &Volume<IO>,
    dir: u64,
    prefix: &str,
) -> Result<BTreeSet<String>, String> {
    let mut here = Vec::new();
    volume
        .readdir(dir, &mut |name, inum, kind| {
            here.push((name.to_string(), inum, kind));
            true
        })
        .map_err(|e| format!("listing {prefix}: {e:?}"))?;

    let mut out = BTreeSet::new();
    for (name, inum, kind) in here {
        let path = format!("{prefix}/{name}");
        // `find -printf %y`'s letters, so the two listings are comparable
        // without either side translating the other's vocabulary.
        let letter = match kind {
            FileKind::Dir => "d",
            FileKind::Regular => "f",
            FileKind::Symlink => "l",
            FileKind::Other(_) => "?",
        };
        out.insert(format!("{letter} {path}"));
        if kind == FileKind::Dir {
            out.extend(walk(volume, inum, &path)?);
        }
    }
    Ok(out)
}
