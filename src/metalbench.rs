//! `toyos-metal` against a machine that runs ToyOS and nothing else: the
//! bench.
//!
//! **The machine keeps one image and tries another once.** The bench image is
//! the slot the table marks: sshd authorizing this host's runner key,
//! `update`, and nothing staged. A boot this loop judges is delivered as
//! `ssh <machine> update --once < image` into the idle slot, which the loader
//! boots at the next reboot and never again (`toyos_update::slots::Request`);
//! the boot runs its job list and hands the machine back, and the machine
//! comes back as the bench, from which the boot's files are read over the
//! same sshd.
//!
//! **The judges are the old path's, over the same two texts**: the loader's
//! passes of this boot — kept by the pass after them as `loader-previous.log`,
//! because the bench's own pass starts a new `loader.log` — and every `logd`
//! file of this boot, told from the bench's own by the ROOT this image mounts.
//! Each is held to this image before it is judged: the loader's by the signed
//! header's digest it verified, the kernel's by the ROOT UUID it names. A
//! volume's raw bytes are the one thing not read: the bench has the log
//! partition mounted, so no read of it here is a quiescent volume, and
//! `--fat32-check` is the old path's alone
//! (`issues/boot-media/the-bench-reads-no-quiescent-log-volume.md`).
//!
//! **One connection where one will do**: the bench's listener resets a
//! connect that lands between two of its accepts
//! (`issues/hardware/a-connect-between-two-accepts-is-reset.md`), so `/log`
//! is read whole in one session, and the session that reads it after the boot
//! is also the event that says the bench is back.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::bootlog;
use crate::metal::{self, Args, Machine, Refusal, Wire};
use crate::metaltalk::Ssh;

/// How long one probe of a machine's sshd waits for the key to be taken: a
/// machine that is up answers in well under a second, and one that is booting
/// refuses the connection outright.
const PROBE_SECS: u64 = 10;

/// Between two asks of a machine this loop is waiting on.
const ASK_EVERY: Duration = Duration::from_secs(2);

/// Where the machine's files are, on the machine.
const LOG_DIR: &str = "/log";

/// One machine reached with one key: the client, the key, and where.
pub struct Bench {
    ssh: Ssh,
    machine: Machine,
    scratch: PathBuf,
}

/// `/log` as one session fetched it onto this host.
struct Logs {
    dir: PathBuf,
    names: Vec<String>,
}

impl Logs {
    /// A file of it, as text, or the refusal that the machine keeps none.
    fn read(&self, name: &str) -> Result<String, Refusal> {
        if !self.names.iter().any(|n| n == name) {
            return Err(Refusal::NotThisBoot(format!("{LOG_DIR} holds no {name}, of {:?}", self.names)));
        }
        let at = self.dir.join(name);
        std::fs::read(&at)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|e| Refusal::File { path: at.display().to_string(), why: e.to_string() })
    }

    /// `logd`'s files, in the order theirs sort.
    fn logd(&self) -> Vec<&str> {
        let mut logd: Vec<&str> =
            self.names.iter().map(String::as_str).filter(|name| bootlog::is_logd_file(name)).collect();
        logd.sort_unstable();
        logd
    }
}

impl Bench {
    /// The client and the key, refused by name where either is missing.
    pub fn prepare(key: &Path, machine: &Machine, scratch: &Path) -> Result<Self, Refusal> {
        std::fs::create_dir_all(scratch)
            .map_err(|e| Refusal::File { path: scratch.display().to_string(), why: e.to_string() })?;
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let ssh = Ssh::at(root, key.to_path_buf()).map_err(Refusal::Cable)?;
        Ok(Self { ssh, machine: machine.clone(), scratch: scratch.to_path_buf() })
    }

    /// Where the machine's sshd is now, or why no address answers for it.
    fn at(&self) -> Result<SocketAddr, Refusal> {
        self.machine.ssh_at().map_err(|why| Refusal::Remote {
            what: "finding the machine".to_string(),
            status: "had no address".to_string(),
            stderr: why,
        })
    }

    /// Run `command` on the machine and hold it to status 0: its output.
    fn exec(&self, what: &str, command: &str) -> Result<String, Refusal> {
        let at = self.at()?;
        let exec = self.ssh.exec(at, command, &self.scratch).map_err(|why| Refusal::Remote {
            what: what.to_string(),
            status: format!("was not answered at {at}"),
            stderr: why,
        })?;
        let said = String::from_utf8_lossy(&exec.stdout).to_string();
        if exec.status != Some(0) {
            return Err(Refusal::Remote { what: what.to_string(), status: format!("ended {:?}", exec.status), stderr: said });
        }
        Ok(said)
    }

    /// The machine's `/log`, whole, into `into` over one session.
    fn fetch(&self, into: &Path) -> Result<Logs, String> {
        let at = self.machine.ssh_at()?;
        let _ = std::fs::remove_dir_all(into);
        let names = self.ssh.fetch(at, LOG_DIR, into)?.into_iter().map(|(name, _)| name).collect();
        Ok(Logs { dir: into.to_path_buf(), names })
    }

    /// Whether the machine takes this key now: a machine not up, one booting,
    /// and a boot that authorizes another key are all "not yet".
    fn answers(&self) -> bool {
        self.machine.ssh_at().is_ok_and(|at| self.ssh.probe(at, PROBE_SECS).is_ok())
    }

    /// Wait until [`Bench::answers`] is `answering`, within `secs`; how long
    /// that took. Each probe is a wait on the machine's own answer, bounded by
    /// [`PROBE_SECS`], and the next is asked [`ASK_EVERY`] after it.
    fn wait(&self, secs: u64, what: &'static str, answering: bool) -> Result<u64, Refusal> {
        let began = std::time::Instant::now();
        while began.elapsed().as_secs() < secs {
            if self.answers() == answering {
                return Ok(began.elapsed().as_secs());
            }
            std::thread::sleep(ASK_EVERY);
        }
        Err(Refusal::Silent { what, secs, last: None })
    }

    /// Wait until the machine's `/log` comes back over this key, within
    /// `secs`: the bench taking the runner key and the boot's files, in the
    /// one session. How long it took, and the files.
    fn wait_for_the_log(&self, secs: u64, what: &'static str, into: &Path, before: &str) -> Result<(u64, Logs), Refusal> {
        let began = std::time::Instant::now();
        let mut last = None;
        while began.elapsed().as_secs() < secs {
            match self.fetch(into) {
                Ok(logs) if rebooted(&logs, before) => return Ok((began.elapsed().as_secs(), logs)),
                Ok(_) => last = Some(format!("its {} is the one fetched before the reboot", bootlog::LOADER_LOG)),
                Err(why) => last = Some(why),
            }
            std::thread::sleep(ASK_EVERY);
        }
        Err(Refusal::Silent { what, secs, last })
    }
}

/// **The bench's loader is the one this image was built with**, or nothing is
/// delivered: an update installs a kernel and ROOT and never the loader, so a
/// boot under another loader is a boot of a different machine than the image
/// describes — and every judge of a loader's own lines would be reading the
/// wrong one. The bench's own pass names its loader by its file's hash.
fn same_loader(logs: &Logs, image: &toyos_update::Digest) -> Result<(), Refusal> {
    let now = logs.read(bootlog::LOADER_LOG)?;
    let said = now
        .lines()
        .find_map(|l| l.split(bootlog::LOADER_IS).nth(1))
        .map(str::trim)
        .ok_or_else(|| Refusal::Undelivered(format!("the bench's {} names no loader: {:?}", bootlog::LOADER_LOG, bootlog::LOADER_IS)))?;
    let mut hex = [0u8; 64];
    let ours = toyos_update::hex(image, &mut hex);
    if said != ours {
        return Err(Refusal::Undelivered(format!(
            "the bench runs the loader {said} and this image was built with {ours}: an update installs no loader, \
             so a boot of this image on this bench would run under a loader it was not built with. Build the \
             bench from this tree and hand the machine to it again (`toyos-metal --via-ubuntu --resident`)"
        )));
    }
    println!("the bench runs this image's own loader, {ours}");
    Ok(())
}

/// The cable's facts off the bench before the boot: the address its sshd
/// answers at, the MAC netd brought up — out of the bench's own log, since no
/// other operating system is there to ask
/// (`issues/boot-media/the-benchs-cable-is-read-by-the-driver-under-test.md`)
/// — and its clock against this one's.
fn wire(bench: &Bench, logs: &Logs, nic: &str) -> Result<Wire, Refusal> {
    let bad = |why: String| Refusal::Wire { nic: nic.to_string(), why };
    let at = bench.at()?;
    let std::net::IpAddr::V4(addr) = at.ip() else {
        return Err(bad(format!("the bench answers at {at}, which is no IPv4 address")));
    };
    // By ROOT, never by name: a name is the bench's clock, which can step back.
    let own = logs.read(bootlog::LOADER_LOG)?;
    let root = own
        .lines()
        .find_map(|l| l.split(bootlog::BOOT_PARAMETER).nth(1))
        .and_then(|param| toyos_abi::boot::root_uuid(param.trim().trim_matches('"')))
        .ok_or_else(|| bad(format!("the bench's {} names no ROOT", bootlog::LOADER_LOG)))?;
    let netd = crate::lan::netd_records(&kernel_log(logs, root, &[])?);
    let mac = netd
        .lines()
        .find_map(|line| line.split(crate::lan::MAC).nth(1))
        .map(|rest| rest.split_whitespace().next().unwrap_or_default().to_ascii_lowercase())
        .ok_or_else(|| bad(format!("the bench's own log carries no {:?} record of netd's", crate::lan::MAC)))?;
    let before = metal::unix_now();
    let said = bench.exec("reading the machine's own clock", "date -u +%s").map_err(|e| bad(e.to_string()))?;
    let skew = metal::clock_skew(before, &said, metal::unix_now()).map_err(bad)?;
    Ok(Wire { iface: "netd".to_string(), addr, mac, skew })
}

/// The logd files of the boot that mounted `root`, in order, as one text; the
/// empty text where no file names it — a boot that never reached `logd`.
///
/// **Told from the bench's own by the ROOT it mounted.** Every boot's first
/// part names its ROOT's filesystem (`kernel/src/rootfs.rs`), and an image's
/// ROOT UUID is its own, so a file is this boot's by its content, never by
/// being the one before the newest.
fn kernel_log(logs: &Logs, root: &str, before: &[String]) -> Result<String, Refusal> {
    let mounted = format!("filesystem {root},");
    let logd = logs.logd();
    let mut stems: Vec<&str> = logd.iter().map(|name| stem(name)).collect();
    stems.dedup();
    for boot in stems.iter().rev() {
        let parts: Vec<&str> = logd.iter().copied().filter(|name| stem(name) == *boot).collect();
        // A name /log held before the delivery is an earlier boot's.
        if parts.iter().any(|part| before.iter().any(|name| name == part)) {
            continue;
        }
        let first = logs.read(parts[0])?;
        if !first.lines().any(|l| l.contains(bootlog::MOUNTED_FROM_MEMORY) && l.contains(&mounted)) {
            continue;
        }
        let mut text = first;
        for part in &parts[1..] {
            text.push_str(&logs.read(part)?);
        }
        return Ok(text);
    }
    Ok(String::new())
}

/// A logd file's boot: its name without the part number and the extension.
fn stem(name: &str) -> &str {
    let bare = name.strip_suffix(".log").unwrap_or(name);
    match bare.rsplit_once('_') {
        Some((stem, part)) if part.len() == 4 && part.bytes().all(|b| b.is_ascii_digit()) => stem,
        _ => bare,
    }
}

/// Whether a pass has run since `before`: every pass starts or appends to it.
fn rebooted(logs: &Logs, before: &str) -> bool {
    logs.read(bootlog::LOADER_LOG).is_ok_and(|now| now != before)
}

/// The loader's passes of this boot: the file the bench's own pass kept of
/// the chain before it, held to the signed header this image carries.
fn loader_log(logs: &Logs, digest: &toyos_update::Digest) -> Result<String, Refusal> {
    let text = logs.read(bootlog::LOADER_PREVIOUS_LOG)?;
    let mut hex = [0u8; 64];
    let named = format!("signed header {} verifies", toyos_update::hex(digest, &mut hex));
    if !text.contains(&named) {
        return Err(Refusal::NotThisBoot(format!(
            "{} does not name this image's signed header ({named:?}): the passes the machine kept are \
             another boot's, and this image's were never the machine's to keep",
            bootlog::LOADER_PREVIOUS_LOG
        )));
    }
    Ok(text)
}

/// What the thread that asked the delivered boot for something came back with.
enum Reached {
    Swapped(Result<(), Refusal>),
    Talked(metal::Heard),
}

/// **One boot, judged**: the image delivered once, the machine rebooted into
/// it, back as the bench, the boot's files read and judged by the old path's
/// judges.
pub fn run(args: &Args, image: &Path, dir: &Path) -> Result<Option<u64>, Refusal> {
    metal::admit(image, &metal::Target::t14()?)?;
    let armed = metal::arms_are_admissible(image)?;
    let update = crate::image::update_of(image)
        .map_err(|why| Refusal::File { path: image.display().to_string(), why })?;
    // Before anything can refuse: a directory left holding the last run's
    // files is one a judge reads as this run's.
    metal::clear_readback(dir)?;
    let mut hex = [0u8; 64];
    println!(
        "image {}: {} bytes as an update, version {}, signed header {}, ROOT {}; armed with {armed:?}",
        image.display(),
        update.bytes.len(),
        update.version,
        toyos_update::hex(&update.digest, &mut hex),
        update.root
    );
    // A swapping boot's key is the swap's: the swap owns the machine's
    // stream, and a conversation beside it would hand the machine back under
    // it.
    let swap = match &args.swap {
        Some(service) => {
            let ask = args.swap_ask(service)?;
            metal::clear_swap(dir)?;
            Some(ask)
        }
        None => None,
    };
    let cable = match (&args.talk, &swap) {
        (Some(key), None) => Some(metal::Talking::prepare(key, dir, &args.machine)?),
        _ => None,
    };
    let bench = Bench::prepare(&args.target.key, &args.machine, &dir.join("bench"))?;

    // The bench before the boot, in one session: that it takes the runner
    // key, which loader it runs, and — for a boot that names its cable — what
    // its netd brought up.
    let at = bench.at()?;
    let before = bench.fetch(&dir.join("bench").join("before")).map_err(|why| Refusal::Remote {
        what: "reading the bench's /log".to_string(),
        status: format!("was not answered at {at}"),
        stderr: why,
    })?;
    same_loader(&before, &update.loader)?;
    let before_loader = before.read(bootlog::LOADER_LOG)?;
    let wire = match &args.nic {
        Some(nic) => {
            let wire = wire(&bench, &before, nic)?;
            println!("the bench holds {} for netd, MAC {}, its clock {} s from this host's", wire.addr, wire.mac, wire.skew);
            Some(wire)
        }
        None => None,
    };

    let sent = dir.join("image.update");
    std::fs::write(&sent, &update.bytes)
        .map_err(|e| Refusal::File { path: sent.display().to_string(), why: e.to_string() })?;
    if args.dry_run {
        println!("  would run: update --once < {} ({} bytes), then reboot", sent.display(), update.bytes.len());
        let _ = std::fs::remove_file(&sent);
        println!("dry run: nothing was written and the machine was not rebooted");
        return Ok(None);
    }
    let delivered = bench.ssh.pipe(at, "update --once", &sent, &bench.scratch);
    let _ = std::fs::remove_file(&sent);
    let delivered = delivered.map_err(|why| Refusal::Undelivered(format!("`update --once` was not answered: {why}")))?;
    let said = String::from_utf8_lossy(&delivered.stdout).to_string();
    let installed = format!("update: installed version {} in slot", update.version);
    if delivered.status != Some(0) || !said.contains(&installed) {
        return Err(Refusal::Undelivered(format!("`update --once` ended {:?} saying {}", delivered.status, said.trim())));
    }
    print!("  {said}");
    let asked = bench.ssh.fire(at, crate::metaltalk::REBOOT).map_err(|why| Refusal::Remote {
        what: "rebooting".to_string(),
        status: "was not answered".to_string(),
        stderr: why,
    })?;
    println!("  `{}` at {at} answered {asked}", crate::metaltalk::REBOOT);

    let by = Duration::from_secs(args.wait_secs);
    bench.wait(metal::GOING_DOWN_SECS, "go down", false)?;
    let ping = wire.as_ref().map(|w| metal::Ping::start(w.addr));
    // **A talking or swapping boot is asked for nothing until it answers as
    // itself**: its sshd taking the key the boot authorizes, which the bench's
    // does not. Until then the machine's name and every forward onto it may
    // be the bench's, and a stream asked for then is the wrong boot's or none.
    let boot_key = swap.as_ref().map(|ask| ask.key).or(cable.as_ref().and(args.talk.as_deref()));
    let (swap, cable) = (&swap, &cable);
    let after = dir.join("bench").join("after");
    let (back, reached) = std::thread::scope(|scope| {
        let reaching = boot_key.map(|key| {
            scope.spawn(move || -> Result<Reached, Refusal> {
                let boot = Bench::prepare(key, &args.machine, &dir.join("boot"))?;
                boot.wait(args.wait_secs, "answer as the delivered boot", true)?;
                match (swap, cable) {
                    (Some(ask), _) => Ok(Reached::Swapped(metal::swap_on(&args.machine, ask, dir, by))),
                    (None, Some(cable)) => Ok(Reached::Talked(match cable.start(by) {
                        Ok((stream, handle)) => {
                            let heard =
                                handle.join().unwrap_or_else(|_| Err("the conversation's thread panicked".to_string()));
                            stream.give_up();
                            (heard, stream.lines())
                        }
                        Err(refused) => (Err(refused.to_string()), Vec::new()),
                    })),
                    (None, None) => unreachable!("a boot key is the swap's or the conversation's"),
                }
            })
        });
        let back = bench.wait_for_the_log(args.wait_secs, "come back", &after, &before_loader);
        let reached = reaching.map(|thread| {
            thread.join().unwrap_or_else(|_| Err(Refusal::Cable("the thread asking the boot panicked".to_string())))
        });
        (back, reached)
    });
    let replied = match ping {
        Some(ping) => ping.end()?,
        None => None,
    };
    let (heard, swapped) = match reached {
        None => (None, None),
        Some(Ok(Reached::Swapped(swapped))) => (None, Some(swapped)),
        Some(Ok(Reached::Talked(heard))) => {
            metal::write_talk(dir, &heard.0)?;
            (Some(heard), None)
        }
        // A boot that never answered as itself had no conversation and no
        // swap, and says so where each would have.
        Some(Err(refused)) => match swap {
            Some(_) => (None, Some(Err(refused))),
            None => {
                let heard: metal::Heard = (Err(refused.to_string()), Vec::new());
                metal::write_talk(dir, &heard.0)?;
                (Some(heard), None)
            }
        },
    };
    let (back, logs) = back?;
    println!("the bench gave back its /log over the runner key {back} s after it went down");

    let loader = loader_log(&logs, &update.digest)?;
    let log = kernel_log(&logs, &update.root, &before.names)?;
    print!("{loader}{log}");
    // The stick is the disk the bench booted from, so it was there before the
    // bench could answer: zero, by construction and not by a reading.
    metal::write_readback(dir, &loader, &log, back, 0, wire.as_ref(), replied)?;
    println!("readback written to {}", dir.display());
    let ms = metal::judge(&armed, &loader, &log, heard.as_ref())?;
    // After the boot's own verdict, as a conversation's is: a boot that
    // never reached its network is named by that verdict first.
    if let Some(swapped) = swapped {
        swapped?;
    }
    Ok(ms)
}

/// **The bench takes the machine**: its sshd, reached with the runner key,
/// asks its loader to put its entry first in `BootOrder`; the reboot writes
/// it; and the pass after says the machine was booted by that entry.
pub fn take_the_machine(key: &Path, machine: &Machine, wait_secs: u64, scratch: &Path) -> Result<(), Refusal> {
    let bench = Bench::prepare(key, machine, scratch)?;
    println!("waiting for the bench to take the runner key");
    let (_, before) = bench.wait_for_the_log(wait_secs, "come up as the bench", &scratch.join("before"), "")?;
    let before = before.read(bootlog::LOADER_LOG)?;
    let said = bench.exec("asking the loader for the boot order", "update --boot-first")?;
    print!("  {said}");
    let at = bench.at()?;
    let asked = bench.ssh.fire(at, crate::metaltalk::REBOOT).map_err(|why| Refusal::Remote {
        what: "rebooting".to_string(),
        status: "was not answered".to_string(),
        stderr: why,
    })?;
    println!("  `{}` at {at} answered {asked}", crate::metaltalk::REBOOT);
    bench.wait(metal::GOING_DOWN_SECS, "go down", false)?;
    let (_, logs) = bench.wait_for_the_log(wait_secs, "come back", &scratch.join("log"), &before)?;
    // The pass that wrote the order ended a chain, so the bench's own pass
    // kept it; the file the bench runs under is the one the order booted.
    let kept = logs.read(bootlog::LOADER_PREVIOUS_LOG)?;
    let first = kept
        .lines()
        .find(|l| l.contains("this loader's ESP") && l.contains("is first"))
        .ok_or_else(|| Refusal::NotThisBoot(format!("no pass wrote the boot order:\n{kept}")))?;
    let now = logs.read(bootlog::LOADER_LOG)?;
    let booted = now.lines().find(|l| l.contains("this pass was booted as")).unwrap_or_default();
    println!("  {}\n  {}", first.trim(), booted.trim());
    println!("TAKEN: the machine boots the bench first, and every judged boot is delivered with `update --once`");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_logd_file_is_named_for_its_boot() {
        assert_eq!(stem("2026-09-06-084003.log"), "2026-09-06-084003");
        assert_eq!(stem("2026-09-06-084003_0002.log"), "2026-09-06-084003");
        assert_eq!(stem("unknown-00_0012.log"), "unknown-00");
        assert_eq!(stem("unknown-00.log"), "unknown-00");
    }

    /// **A boot's kernel log is the file that names its ROOT**, never the one
    /// before the newest: the bench's own boot, a boot that never reached
    /// `logd`, and an earlier boot of another image each name another.
    #[test]
    fn a_boots_log_is_the_one_that_names_its_root() {
        let scratch = toyos_tmpdir::TempDir::new("logs");
        let dir = scratch.to_path_buf();
        let mounted = |root: &str| format!("[kernel 0.2 cpu0] {} 0x1+0x2, filesystem {root}, 9 blocks\n", bootlog::MOUNTED_FROM_MEMORY);
        let files = [
            ("2026-09-27-100000.log", mounted("aaaa")),
            ("2026-09-27-110000.log", mounted("bbbb")),
            ("2026-09-27-110000_0002.log", "the boot's second part\n".to_string()),
            ("2026-09-27-120000.log", mounted("cccc")),
        ];
        for (name, text) in &files {
            std::fs::write(dir.join(name), text).expect("a staged file");
        }
        let mut names: Vec<String> = files.iter().map(|(n, _)| (*n).to_string()).collect();
        names.push("loader.log".to_string());
        let logs = Logs { dir, names };
        let text = kernel_log(&logs, "bbbb", &[]).expect("a read");
        assert!(text.contains("filesystem bbbb,") && text.ends_with("the boot's second part\n"), "{text}");
        assert_eq!(kernel_log(&logs, "dddd", &[]).expect("a read"), "", "a boot that reached no logd");
        assert!(matches!(logs.read("loader-previous.log"), Err(Refusal::NotThisBoot(_))));
        let before = vec!["2026-09-27-110000_0002.log".to_string()];
        assert_eq!(kernel_log(&logs, "bbbb", &before).expect("a read"), "", "an earlier delivery's boot");
    }

    #[test]
    fn a_readback_is_of_a_pass_since_and_of_this_image() {
        let scratch = toyos_tmpdir::TempDir::new("back");
        let dir = scratch.to_path_buf();
        let digest = [0x5Au8; 32];
        let mut hex = [0u8; 64];
        let ours = format!("Slot B: signed header {} verifies\n", toyos_update::hex(&digest, &mut hex));
        std::fs::write(dir.join(bootlog::LOADER_LOG), "the bench's own pass\n").expect("a staged file");
        std::fs::write(dir.join(bootlog::LOADER_PREVIOUS_LOG), &ours).expect("a staged file");
        let names = vec![bootlog::LOADER_LOG.to_string(), bootlog::LOADER_PREVIOUS_LOG.to_string()];
        let logs = Logs { dir: dir.clone(), names };
        assert!(!rebooted(&logs, "the bench's own pass\n"), "the same loader.log is the bench not yet rebooted");
        assert!(rebooted(&logs, "the pass before\n"));
        assert!(!rebooted(&Logs { dir, names: Vec::new() }, ""), "no loader.log at all");
        assert_eq!(loader_log(&logs, &digest).expect("this image's passes"), ours);
        assert!(matches!(loader_log(&logs, &[0x5B; 32]), Err(Refusal::NotThisBoot(_))), "an earlier boot's passes");
    }

    #[test]
    fn a_machine_that_never_gives_back_its_log_is_refused_with_the_last_ask() {
        let scratch = toyos_tmpdir::TempDir::new("silent");
        let root = scratch.to_path_buf();
        let client = crate::build::ssh_client_host(&root);
        std::fs::create_dir_all(client.parent().expect("a directory")).expect("the client's directory");
        std::fs::write(&client, b"").expect("a staged client");
        std::fs::write(root.join("key"), b"").expect("a staged key");
        let log = crate::metaltalk::Peer::Named { host: String::new(), port: 1 };
        let bench = Bench {
            ssh: Ssh::at(&root, root.join("key")).expect("a client and a key"),
            machine: Machine { log, ssh: None },
            scratch: root.clone(),
        };
        let refused = bench.wait_for_the_log(1, "come back", &root.join("log"), "").err().expect("no machine");
        assert!(refused.to_string().contains("the last ask of it:  did not resolve"), "{refused}");
    }

    /// **A bench under another loader takes no image**: the line its pass
    /// wrote names the loader it runs, and an image built with another is
    /// refused before it is delivered — as is a bench whose pass named none.
    #[test]
    fn an_image_is_delivered_only_to_the_loader_it_was_built_with() {
        let scratch = toyos_tmpdir::TempDir::new("loader");
        let dir = scratch.to_path_buf();
        let ours = [0x11u8; 32];
        let mut hex = [0u8; 64];
        let line = format!("ToyOS Bootloader 1.0\n{} {}\n", bootlog::LOADER_IS, toyos_update::hex(&ours, &mut hex));
        std::fs::write(dir.join(bootlog::LOADER_LOG), line).expect("a staged file");
        let logs = Logs { dir: dir.clone(), names: vec![bootlog::LOADER_LOG.to_string()] };
        assert_eq!(same_loader(&logs, &ours), Ok(()));
        assert!(matches!(same_loader(&logs, &[0x12; 32]), Err(Refusal::Undelivered(_))));
        std::fs::write(dir.join(bootlog::LOADER_LOG), "ToyOS Bootloader 1.0\n").expect("a staged file");
        assert!(matches!(same_loader(&logs, &ours), Err(Refusal::Undelivered(_))), "a pass that named no loader");
    }
}
