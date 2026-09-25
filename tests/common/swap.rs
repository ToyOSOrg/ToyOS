//! A running service's binary replaced with no reboot, rehearsed in QEMU: netd
//! swapped for its own rebuild while `logd` streams to this host and sshd
//! carries the ask — and the three ways a swap must leave the old service
//! running.
//!
//! **The machine's own `/log` is the oracle**, read off the image behind the
//! guest's back once `reboot` over ssh has ended the boot: one `Boot:
//! complete` in it is the claim that nothing rebooted, the kernel's `spawn:`
//! record names the binary it loaded, and the stream's lines must be the
//! file's own in its order. What the host heard ([`metalswap::judge`]) is the
//! same reading the T14 run gets.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use toyos_build::bootlog;
use toyos_build::metalswap::{self, Ask, Expect, Swapped};
use toyos_build::metaltalk::Ssh;
use toyos_swap::Word;

use super::lan::TalkBoot;
use super::logstream::Bench;
use super::qemu::{self, BootOptions, QemuInstance};
use super::serial;

/// The rehearsal on virtio-net: the brief's own machine, and the one whose
/// driver comes up the fastest.
pub const VIRTIO: Bench =
    Bench { profile: qemu::Profile::Headless, config: "tests/swapcase", device: "virtio-net" };

/// A liveness guard on a guest that stopped talking, never a verdict.
const CEILING: Duration = Duration::from_secs(120);

/// The swapping boot's one job on the T14: it holds the machine until the
/// swap invocation hands it back.
const HOLD: &str = "test_rs_lan_swap_hold";
pub const HOLD_JOBS: &[&str] = &[HOLD];

/// A test binary that panics the instant it starts, and its panic's own line.
const CRASH: &str = "swap_crash";
const CRASH_PANIC: &str = "panicked at src/bin/swap_crash.rs";

/// A booted talking guest with its ssh forward, and the client to reach it.
struct Rig {
    staged: TalkBoot,
    guest: QemuInstance,
    console: String,
    ssh: Ssh,
    forward: SocketAddr,
}

impl Rig {
    /// `binary` sent as netd's replacement once sshd answers, and the
    /// machine's word on it: a boot's stream opens when netd leases, and sshd
    /// may still be binding then.
    fn swap_once_sshd_answers(
        &self,
        binary: &Path,
        digest: &toyos_swap::Digest,
    ) -> Result<Result<String, String>, String> {
        self.staged.stream.wait_connected(CEILING).ok_or("the boot never opened its stream")?;
        let asked = std::time::Instant::now();
        loop {
            match self.ssh.swap(self.forward, "netd", binary, digest) {
                Err(why) if asked.elapsed() < Duration::from_secs(30) => {
                    eprintln!("  [swap] not taken yet: {why}");
                    std::thread::sleep(Duration::from_secs(1));
                }
                answered => return Ok(answered),
            }
        }
    }

    fn boot(name: &str, bench: Bench) -> Result<Self, String> {
        Self::boot_armed(name, bench, &[])
    }

    /// [`Rig::boot`] on the test kernel, with `actuators` armed.
    fn boot_armed(name: &str, bench: Bench, actuators: &'static [&'static str]) -> Result<Self, String> {
        let staged = TalkBoot::stage_armed(name, bench, actuators)?;
        let ssh_port = qemu::free_host_port();
        let options = BootOptions { ssh_port: Some(ssh_port), ..staged.options() };
        let guest = QemuInstance::boot_with_options(&staged.case, &[], &[], options);
        let console = guest.boot_log().to_string();
        let ssh = Ssh::at(&super::compile::repo_root(), staged.identity.private().to_path_buf())?;
        let forward = SocketAddr::from((Ipv4Addr::LOCALHOST, ssh_port));
        Ok(Self { staged, guest, console, ssh, forward })
    }

    fn swap(&self, service: &str, binary: &Path, named: Option<toyos_swap::Digest>) -> Result<Swapped, String> {
        let swapped = metalswap::swap(
            &self.staged.stream,
            &self.ssh,
            Some(self.forward),
            &Ask { service, binary, named },
            CEILING,
            &self.staged.scratch,
        )?;
        for (word, detail) in &swapped.words {
            eprintln!("  [swap] init: {}: {detail}", word.as_str());
        }
        Ok(swapped)
    }

    /// `metalswap::judge`'s verdict, and the rig back for what follows it.
    fn judged(self, swapped: &Swapped, expect: Expect) -> Result<Self, String> {
        match metalswap::judge(swapped, expect) {
            Ok(said) => {
                said.iter().for_each(|line| eprintln!("  [swap] {line}"));
                Ok(self)
            }
            Err(bad) => Err(self.fail(format!("{} finding(s):\n  {}", bad.len(), bad.join("\n  ")))),
        }
    }

    /// `why`, where the guest's whole console is kept, and what `logd` and
    /// init wrote about the stream and the swap into the `/log` the guest
    /// leaves when it is stopped here.
    fn fail(mut self, why: String) -> String {
        self.console.push_str(&self.guest.drain_serial(Duration::from_secs(2)));
        drop(self.guest);
        let at = self.staged.scratch.join("console.log");
        let kept = match std::fs::write(&at, &self.console) {
            Ok(()) => format!("the guest's console is {}", at.display()),
            Err(e) => format!("the guest's console could not be kept at {}: {e}", at.display()),
        };
        let said = match super::volumes::whole_log(&self.staged.image, self.staged.start, self.staged.len) {
            Ok(file) => file
                .into_iter()
                .filter(|l| l.contains("logd: ") || l.contains("init: swap ") || l.contains("pcidev: "))
                .collect::<Vec<_>>()
                .concat(),
            Err(e) => format!("/log could not be read: {e}\n"),
        };
        format!("{why}\n  {kept}\n  /log's own lines about the stream and the swap:\n{said}")
    }

    /// End the boot so `/log` is whole — `reboot` over ssh, asked as a program
    /// whose connection is held until the machine goes, so sshd never ends it
    /// for a client that left — and answer the file. `staged` is the one
    /// program panic the test caused on purpose, which the console must carry
    /// exactly once.
    fn finish(mut self, staged: Option<&str>) -> Result<(Vec<String>, Vec<String>, TalkBoot), String> {
        let asked = self.ssh.exec(self.forward, toyos_build::metaltalk::REBOOT, &self.staged.scratch);
        eprintln!("  [swap] `reboot` {:?}", asked.map(|exec| exec.status));
        if let Err(why) =
            qemu::await_marker(&mut self.guest, &mut self.console, bootlog::REBOOTING, "`reboot` over ssh")
        {
            return Err(self.fail(why));
        }
        drop(self.guest);
        // A program's panic prints the spelling a kernel panic does, so the
        // one this test staged is taken out by its own line, and counted.
        let mut console = self.console.clone();
        if let Some(needle) = staged {
            let seen = console.matches(needle).count();
            if seen != 1 {
                return Err(format!("{needle:?} is on the console {seen} time(s), where it was staged once"));
            }
            console = console.lines().filter(|l| !l.contains(needle)).collect::<Vec<_>>().join("\n");
        }
        serial::Serial::named("the swapping boot", console.as_str()).must_be_clean()?;
        let file = super::volumes::whole_log(&self.staged.image, self.staged.start, self.staged.len)?;
        let streamed = self.staged.stream.lines();
        super::logstream::is_prefix_of(&streamed, &file)?;
        let boots = file.iter().filter(|l| l.contains("Boot: complete")).count();
        if boots != 1 {
            return Err(format!("/log holds {boots} `Boot: complete` record(s), where one boot owes one"));
        }
        Ok((file, streamed, self.staged))
    }
}

/// A copy of the build's own `name` binary in `dir`, which is what a swap
/// rehearsal sends as that service's rebuild.
fn rebuilt(name: &str, dir: &Path) -> Result<std::path::PathBuf, String> {
    let to = dir.join(format!("{name}.rebuilt"));
    toyos_build::build::copy_guest_program(&super::compile::repo_root(), name, &to)?;
    Ok(to)
}

/// The first line in `file` holding `needle` at or after index `from`.
fn after(file: &[String], from: usize, needle: &str) -> Option<usize> {
    file[from.min(file.len())..].iter().position(|l| l.contains(needle)).map(|at| from + at)
}

/// netd swapped for its rebuild on `bench`, and the lease coming back through
/// the new process with no reboot between.
fn netd_in_service(name: &str, bench: Bench) -> Result<(), String> {
    let rig = Rig::boot(name, bench)?;
    let binary = rebuilt("netd", &rig.staged.scratch)?;
    let swapped = rig.swap("netd", &binary, None)?;
    let rig = rig.judged(&swapped, Expect::InService)?;
    if !swapped.said.iter().any(|l| l.contains(toyos_build::lan::LEASE)) {
        return Err(format!(
            "the stream carried no lease from netd after the swap; netd said {:?}",
            swapped.said
        ));
    }
    let digest = toyos_swap::parse_hex(&swapped.digest).ok_or("the digest the host sent")?;
    let installed = toyos_swap::installed_path("netd", &digest);
    let (file, _, staged) = rig.finish(None)?;
    // The kernel's own record of what it loaded, then init putting it in
    // service, then a lease from the network after both.
    let spawned = after(&file, 0, &format!("spawn: {installed}"))
        .ok_or_else(|| format!("/log has no `spawn: {installed}` record"))?;
    let committed = after(&file, spawned, &toyos_swap::said("netd", Word::InService, &installed))
        .ok_or("/log has no `in service` from init after the spawn")?;
    let leased = after(&file, spawned, toyos_build::lan::LEASE)
        .ok_or("/log has no lease after the new netd was spawned")?;
    eprintln!(
        "  [swap] /log: spawn at line {spawned}, in service at {committed}, lease at {leased}; \
         one boot ({} lines)",
        file.len()
    );
    let _ = std::fs::remove_file(&staged.image);
    Ok(())
}

pub fn swap_netd(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    netd_in_service("swap-netd", VIRTIO)
}

/// The T14's swap rehearsed on its register file: QEMU's 82574 brought up a
/// second time in one boot by a second netd.
pub fn lan_swap(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    netd_in_service("lan-swap", super::lan::TALK_BENCH)
}

/// **Two asks that must change nothing**: the right binary under the wrong
/// digest, and the right binary under the right digest from a key the image
/// does not authorize. Each leaves netd as it was — no `stopping` word, the
/// machine answering over the same netd — and `/log` shows netd spawned once.
pub fn swap_refusals(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let rig = Rig::boot("swap-refusals", VIRTIO)?;
    let binary = rebuilt("netd", &rig.staged.scratch)?;

    let mut wrong = toyos_swap::digest(&std::fs::read(&binary).map_err(|e| e.to_string())?);
    wrong[0] ^= 1;
    let swapped = rig.swap("netd", &binary, Some(wrong))?;
    let rig = rig.judged(&swapped, Expect::Refused)?;
    if !swapped.answer.as_deref().unwrap_or("").contains("hashes to") {
        return Err(format!("the wrong digest was refused as {:?}, not for its hash", swapped.answer));
    }

    let stranger = super::ssh::Identity::mint(super::ssh::STRANGER_KEY)?;
    let outsider = Ssh::at(&super::compile::repo_root(), stranger.private().to_path_buf())?;
    let digest = toyos_swap::digest(&std::fs::read(&binary).map_err(|e| e.to_string())?);
    match outsider.swap(rig.forward, "netd", &binary, &digest) {
        Err(why) if why.contains("refused this key") => {
            eprintln!("  [swap] a key the image does not authorize: {why}")
        }
        other => return Err(format!("a stranger's swap was answered {other:?}")),
    }
    let (file, streamed, staged) = rig.finish(None)?;
    let stopped: Vec<&String> =
        streamed.iter().chain(&file).filter(|l| toyos_swap::heard(l, "netd").is_some_and(|(w, _)| w == Word::Stopping)).collect();
    if !stopped.is_empty() {
        return Err(format!("init stopped netd for a refused swap: {stopped:?}"));
    }
    let spawns = file.iter().filter(|l| l.contains("spawn: ") && l.contains("/netd")).count();
    if spawns != 1 {
        return Err(format!("/log records {spawns} spawn(s) of netd where the boot's own is the only one owed"));
    }
    let _ = std::fs::remove_file(&staged.image);
    Ok(())
}

/// A replacement that panics at once: init says it failed, starts the binary it
/// replaced, and the machine answers ssh through that one.
pub fn swap_crash_rolls_back(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let rig = Rig::boot("swap-crash", VIRTIO)?;
    let (_, crash) = rust_bins
        .iter()
        .find(|(name, _)| name == CRASH)
        .ok_or_else(|| format!("no `{CRASH}` among the test binaries"))?;
    let binary = rig.staged.scratch.join(CRASH);
    std::fs::write(&binary, crash).map_err(|e| format!("{}: {e}", binary.display()))?;
    let swapped = rig.swap("netd", &binary, None)?;
    let rig = rig.judged(&swapped, Expect::Restored)?;
    let (file, _, staged) = rig.finish(Some(CRASH_PANIC))?;
    let restored = after(&file, 0, &toyos_swap::said("netd", Word::Restored, "/system/bin/netd as pid"))
        .ok_or("/log has no `restored` of the image's netd")?;
    let pid = file[restored]
        .rsplit("as pid ")
        .next()
        .and_then(|pid| pid.trim().parse::<u32>().ok())
        .ok_or_else(|| format!("init's `restored` names no pid: {:?}", file[restored]))?;
    // The lease is looked for after the kernel's spawn of the restored process
    // and not after init's word on it: init speaks once the spawn returns, and
    // a netd that leases first puts its lease above that word.
    let spawned = after(&file, 0, &format!("spawn: /system/bin/netd pid={pid} "))
        .ok_or_else(|| format!("/log has no `spawn:` of the restored netd, pid {pid}"))?;
    after(&file, spawned, toyos_build::lan::LEASE)
        .ok_or("/log has no lease from the restored netd")?;
    let _ = std::fs::remove_file(&staged.image);
    Ok(())
}

/// The T14's swap, judged: what the swap invocation heard over the cable, held
/// against the stick's own `/log` — which came back over a different path and
/// is the oracle for all of it. One `Boot: complete` in the file is the claim
/// that nothing rebooted between the two netds.
pub fn swapped_on_metal(back: &super::metal::Readback) -> Result<(), String> {
    let (swapped, stream) = back.swap()?;
    let mut bad: Vec<String> = Vec::new();
    match metalswap::judge(&swapped, Expect::InService) {
        Ok(said) => said.iter().for_each(|line| eprintln!("  [swap] {line}")),
        Err(found) => bad.extend(found),
    }
    let file: Vec<String> = back.log().text().split_inclusive('\n').map(str::to_string).collect();
    if let Err(why) = super::logstream::is_prefix_of(&stream, &file) {
        bad.push(why);
    }
    let boots = file.iter().filter(|l| l.contains("Boot: complete")).count();
    if boots != 1 {
        bad.push(format!("the stick's log holds {boots} `Boot: complete` record(s), where one boot owes one"));
    }
    match toyos_swap::parse_hex(&swapped.digest) {
        Some(digest) => {
            let installed = toyos_swap::installed_path(&swapped.service, &digest);
            match after(&file, 0, &format!("spawn: {installed}")) {
                Some(spawned) => match after(&file, spawned, toyos_build::lan::LEASE) {
                    Some(leased) => eprintln!(
                        "  [swap] the stick: {installed} spawned at line {spawned}, a lease after it at {leased}"
                    ),
                    None => bad.push(format!("the stick's log has no lease after {installed} was spawned")),
                },
                None => bad.push(format!("the stick's log has no `spawn: {installed}` record")),
            }
        }
        None => bad.push(format!("the swap file's digest {:?} is no digest", swapped.digest)),
    }
    // The host said it was done: its `reboot` ended the boot, not the
    // runner's bound over a hold that never ends on its own.
    if file.iter().any(|l| l.contains(toyos_build::bootlog::JOB_DEADLINE_SAID)) {
        bad.push(format!(
            "the runner's bound ended the boot inside {HOLD}, so the swap invocation never \
             handed the machine back"
        ));
    }
    if bad.is_empty() {
        return Ok(());
    }
    Err(format!("{} finding(s):\n  {}", bad.len(), bad.join("\n  ")))
}

/// The replacement a DMA control swaps in: it stops the 82574 the way netd does
/// before its first grant, and does nothing else.
const IDLE: &str = "swap_claim_idle";

/// Its line once the part is mastering, which opens the window.
const HOLDING: &str = "swap_claim_idle: holding the NIC mastering";

/// Connects to the forward that slirp's listener completed inside the window,
/// each one a SYN slirp sends the guest's address: the window closes on the
/// last of them.
const KNOCKS: usize = 25;

/// A liveness guard on those connects, never a verdict.
const KNOCKS_WITHIN: Duration = Duration::from_secs(30);

/// **The part keeps running across a release, and the next holder stops it
/// before its first grant.** netd on QEMU's 82574 is swapped for a program that
/// takes the function, reports the receive and transmit enables it inherited,
/// runs `toyos_i219::quiesce` — netd's own first act — and then masters with
/// one grant, and holds it until it is killed. This host then sends the guest
/// [`KNOCKS`] SYNs through slirp. The verdict is the kernel's console saying
/// the unit saw no DMA fault.
///
/// **The kernel's reset is not this test's subject**: the 82574 advertises
/// the D3hot round trip and QEMU does not reset it on one — measured, the
/// inherited `RCTL` still has receive enabled. So a holder that skipped the
/// quiesce faults here (the negative control); [`swap_resets_the_function`] is
/// the reset's.
pub fn swap_quiets_the_function(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    let mut rig = Rig::boot("swap-quiet", super::lan::TALK_BENCH)?;
    let idle = test_binary(rust_bins, IDLE)?;
    let binary = rig.staged.scratch.join(IDLE);
    std::fs::write(&binary, idle).map_err(|e| format!("{}: {e}", binary.display()))?;
    let answer = rig.swap_once_sshd_answers(&binary, &toyos_swap::digest(idle))?;
    eprintln!("  [swap] the swap was answered {answer:?}");
    if let Err(why) = init_accepted(&answer) {
        return Err(rig.fail(why));
    }
    let held = qemu::await_marker(&mut rig.guest, &mut rig.console, HOLDING, "the replacement holding the part mastering");
    if let Err(why) = held {
        return Err(rig.fail(why));
    }
    // From the holding line on, so every frame is the replacement's to stop.
    let (stop, taken) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)));
    let knocking = {
        let (stop, taken, at) = (Arc::clone(&stop), Arc::clone(&taken), rig.forward);
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                if std::net::TcpStream::connect_timeout(&at, Duration::from_millis(200)).is_ok() {
                    taken.fetch_add(1, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    };
    let asked = std::time::Instant::now();
    let knocked = qemu::await_guest(&mut rig.guest, &mut rig.console, "the host's frames at the part", |_| {
        taken.load(Ordering::SeqCst) >= KNOCKS || asked.elapsed() > KNOCKS_WITHIN
    });
    stop.store(true, Ordering::SeqCst);
    let _ = knocking.join();
    if let Err(why) = knocked {
        return Err(rig.fail(why));
    }
    let taken = taken.load(Ordering::SeqCst);
    if taken < KNOCKS {
        return Err(rig.fail(format!(
            "slirp took {taken} of {KNOCKS} connects in {KNOCKS_WITHIN:?}, so the window held too few \
             frames for a clean console to mean the part was stopped"
        )));
    }
    let text = rig.console.clone();
    let console = serial::Serial::named("the quieting boot", text.as_str());
    let released = console.must_say("released from slot 0; reset by")?.to_string();
    let inherited = console.must_say("swap_claim_idle: inherited")?.to_string();
    if let Err(why) = console.must_be_clean() {
        return Err(rig.fail(format!("{why}\n  the release said: {}", released.trim_end())));
    }
    eprintln!(
        "  [swap] {}; {}; the next holder stopped it, mastered it through {taken} SYNs in {:?}, and \
         the unit saw no fault",
        released.trim_end(),
        inherited.trim_end(),
        asked.elapsed()
    );
    drop(rig.guest);
    let _ = std::fs::remove_file(&rig.staged.image);
    Ok(())
}

/// `name` among the build's test binaries.
fn test_binary<'a>(rust_bins: &'a [(String, Vec<u8>)], name: &str) -> Result<&'a [u8], String> {
    rust_bins
        .iter()
        .find(|(bin, _)| bin == name)
        .map(|(_, bytes)| bytes.as_slice())
        .ok_or_else(|| format!("no `{name}` among the test binaries"))
}

/// Nothing after a swap that never reached init is init's to say, so a test
/// waiting on init's words first holds the answer to init's `accepted`.
fn init_accepted(answer: &Result<String, String>) -> Result<(), String> {
    match answer {
        Ok(said) if said.starts_with("accepted ") => Ok(()),
        other => Err(format!("the swap was answered {other:?}, where init's `accepted` is owed")),
    }
}

/// The machine with QEMU's `igb` beside the 82574, netd holding both.
const IGB_BENCH: Bench =
    Bench { profile: qemu::Profile::E1000eBesideIgb, config: "tests/flrswapcase", device: "igb" };

/// The replacement that reads the `igb` through its claim's window.
const FLR_PROBE: &str = "swap_flr_probe";

/// **A function reset on release decodes where its next holder maps it.** netd
/// holds QEMU's `igb`, which resets by an Express function level reset — every
/// BAR back to 0 (PCIe §6.6.2). Swapping netd releases it, and the replacement
/// claims it and reads dword 0 through the window its claim maps: the dword the
/// kernel settled that window against when it first placed it, so all-zeroes
/// or all-ones there is a window the function does not decode.
///
/// The premise is asked of the kernel's own release record, so a function that
/// stopped resetting cannot pass this vacuously.
pub fn swap_resets_the_function(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let mut rig = Rig::boot("swap-reset", IGB_BENCH)?;
    let probe = test_binary(rust_bins, FLR_PROBE)?;
    let binary = rig.staged.scratch.join(FLR_PROBE);
    std::fs::write(&binary, probe).map_err(|e| format!("{}: {e}", binary.display()))?;
    let answer = rig.swap_once_sshd_answers(&binary, &toyos_swap::digest(probe))?;
    eprintln!("  [swap] the swap was answered {answer:?}");
    if let Err(why) = init_accepted(&answer) {
        return Err(rig.fail(why));
    }
    // The probe's read, or a line that says there will be none.
    let failed = toyos_swap::said("netd", Word::Failed, "");
    let held = qemu::await_guest(&mut rig.guest, &mut rig.console, "the replacement reading the igb", |c| {
        c.contains("swap_flr_probe: igb BAR")
            || c.contains("swap_flr_probe: started holding no igb")
            || c.contains(&failed)
    });
    if let Err(why) = held {
        return Err(rig.fail(why));
    }
    let text = rig.console.clone();
    let judged = (|| {
        let console = serial::Serial::named("the resetting boot", text.as_str());
        let released = console.must_say("[8086:10c9] released from slot")?;
        if !released.contains("reset by a function level reset (Express)") {
            return Err(format!("the premise: the igb was not released by an Express FLR — {released}"));
        }
        let read = console.must_say("swap_flr_probe: igb BAR")?;
        let dword = read
            .rsplit("answers 0x")
            .next()
            .and_then(|hex| u32::from_str_radix(hex.trim(), 16).ok())
            .ok_or_else(|| format!("the probe's line carries no dword: {read:?}"))?;
        if dword == 0 || dword == u32::MAX {
            return Err(format!("the reset igb does not decode where its claim maps it: {read}"));
        }
        console.must_be_clean()?;
        eprintln!("  [swap] {}; {}", released.trim_end(), read.trim_end());
        Ok(())
    })();
    if let Err(why) = judged {
        return Err(rig.fail(why));
    }
    drop(rig.guest);
    let _ = std::fs::remove_file(&rig.staged.image);
    Ok(())
}

/// The actuator that puts back none of a reset function's windows but its
/// MSI-X table's.
const BAR_LOST: &[&str] = &["pcidev-bar-lost-on-reset"];

/// The actuator that puts a reset function's BAR 0 back one BAR's size above
/// the window it was cut, inside it.
const BAR_MOVED: &[&str] = &["pcidev-bar-moved-on-reset"];

/// **A replacement refused a device the process it replaces held fails the
/// swap.** netd holds the 82574 and QEMU's `igb`; the `igb` resets on release,
/// and the actuator leaves its register window where the reset put it, so the
/// kernel refuses the next claim of it by name. netd's own rebuild is sent:
/// init must answer `failed` naming the `igb` rather than start a netd without
/// it, find the binary it replaced refused the same device, and close the
/// service — never `in service` over a netd running on the 82574 alone.
pub fn swap_refused_device_fails(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    refused_device_fails("swap-refused-device", BAR_LOST)
}

/// [`swap_refused_device_fails`] with the `igb`'s BAR 0 holding a decodable
/// address that is not its cut: the kernel reads the register's address, not
/// only whether it holds one.
pub fn swap_moved_device_fails(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    refused_device_fails("swap-moved-device", BAR_MOVED)
}

/// The `igb`'s next claim refused under `actuators`, and init's swap failing
/// on it and closing netd for it: the `gone` names the `igb`, so a claim init
/// kept from the refused start and could not mint again ends it otherwise.
fn refused_device_fails(name: &str, actuators: &'static [&'static str]) -> Result<(), String> {
    let mut rig = Rig::boot_armed(name, IGB_BENCH, actuators)?;
    let binary = rebuilt("netd", &rig.staged.scratch)?;
    let digest = toyos_swap::digest(&std::fs::read(&binary).map_err(|e| e.to_string())?);
    let answer = rig.swap_once_sshd_answers(&binary, &digest)?;
    eprintln!("  [swap] the swap was answered {answer:?}");
    if let Err(why) = init_accepted(&answer) {
        return Err(rig.fail(why));
    }
    // The service carrying the stream is the one swapped, so init's words are
    // read off the console: its last one on this swap, whichever it is.
    let [gone, in_service, restored, started, failed] =
        [Word::Gone, Word::InService, Word::Restored, Word::Started, Word::Failed]
            .map(|w| toyos_swap::said("netd", w, ""));
    let ended = qemu::await_guest(&mut rig.guest, &mut rig.console, "init's last word on the swap", |c| {
        c.contains(&gone) || c.contains(&in_service) || c.contains(&restored)
    });
    if let Err(why) = ended {
        return Err(rig.fail(why));
    }
    let text = rig.console.clone();
    let judged = (|| {
        let console = serial::Serial::named("the refusing boot", text.as_str());
        let released = console.must_say("[8086:10c9] released from slot")?;
        if !released.contains("reset by a function level reset (Express)") {
            return Err(format!("the premise: the igb was not released by an Express FLR — {released}"));
        }
        for word in [&in_service, &started, &restored] {
            if let Some(line) = text.lines().find(|l| l.contains(word.as_str())) {
                return Err(format!(
                    "init started a netd though the igb the one it replaced held no longer holds \
                     the window it was cut for: {line}"
                ));
            }
        }
        let refused = console.must_say("no longer holds the window it was cut for")?;
        if !refused.contains("NOT HANDED OVER") {
            return Err(format!("the kernel's refusal is not a refused hand-over: {refused}"));
        }
        let said = console.must_say(&failed)?;
        if !said.contains("pci:8086:10c9") || !said.contains("the process it replaces held it") {
            return Err(format!("init's `failed` does not name the device it could not give: {said}"));
        }
        let closed = console.must_say(&gone)?;
        if !closed.contains("pci:8086:10c9") {
            return Err(format!("init's `gone` does not name the igb the binary it replaced was refused: {closed}"));
        }
        console.must_be_clean()?;
        eprintln!("  [swap] {}; {}; {}", refused.trim_end(), said.trim_end(), closed.trim_end());
        Ok(())
    })();
    if let Err(why) = judged {
        return Err(rig.fail(why));
    }
    drop(rig.guest);
    let _ = std::fs::remove_file(&rig.staged.image);
    Ok(())
}

/// The program [`swap_not_inherited`] runs undeclared.
const PROBE: &str = "swap_probe";

/// **The swap port is not inherited by what sshd runs.** A program the manifest
/// does not declare is uploaded over sftp and run over ssh, so std spawns it
/// holding a duplicate of sshd's namespace; it asks that namespace for `netd`
/// — the premise that it inherited one at all — and for the swap port, and
/// sends init a frame that is no swap request if it gets one. Its exit is the
/// verdict: 0 is the port out of reach, 1 is init reached.
pub fn swap_not_inherited(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let rig = Rig::boot("swap-not-inherited", VIRTIO)?;
    let probe = test_binary(rust_bins, PROBE)?;
    let remote = format!("/tmp/{PROBE}");
    let (host, port) = (super::ssh::HOST, rig.forward.port());
    rig.staged.stream.wait_connected(CEILING).ok_or("the boot never opened its stream")?;
    let asked = std::time::Instant::now();
    loop {
        match super::ssh::ssh_put(host, port, &rig.staged.identity, &remote, probe) {
            Err(why) if asked.elapsed() < Duration::from_secs(30) => {
                eprintln!("  [swap] not taken yet: {why}");
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(why) => return Err(rig.fail(why)),
            Ok(()) => break,
        }
    }
    let command = format!("{remote} {} {} {}", toyos_swap::PORT, toyos_swap::LABEL, toyos_swap::MSG_SWAP);
    let ran = match super::ssh::ssh_exec(host, port, &rig.staged.identity, &command) {
        Ok(ran) => ran,
        Err(why) => return Err(rig.fail(why)),
    };
    let said = format!("{}{}", ran.stdout_text(), ran.stderr_text());
    if ran.status != Some(0) || !said.contains("is not in the namespace it inherited") {
        return Err(rig.fail(format!("{PROBE} ended {:?} saying {said:?}", ran.status)));
    }
    eprintln!("  [swap] {}", said.trim_end());
    let (_, _, staged) = rig.finish(None)?;
    let _ = std::fs::remove_file(&staged.image);
    Ok(())
}
