//! The record stream's judge: a listener on the host, a guest booted with its
//! address on the parameter line, and the guest's own `/log` as the oracle.
//!
//! **The file is what the stream is judged against.** `logd` writes a line to
//! `/log` and then offers the same line to the stream, so a listener that kept
//! up received that file's own first lines and nothing else ([`is_prefix_of`]).
//! The listener is a second, independent reading of the same boot: one arrives
//! over a NIC driver, netd's TCP stack and slirp, the other is read off the FAT
//! volume behind the guest's back. A driver that truncates, reorders or
//! duplicates is a disagreement between them rather than a smaller number
//! nobody reads.
//!
//! The listener is `std::net` in this process and not a program of its own.
//! `tests/https-server-host` is a separate binary because it has to serve TLS
//! from a minted CA; a listener that appends lines to a file is a thread.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::qemu::{self, BootOptions, QemuInstance};
use super::{compile, serial, volumes};

/// How long a boot has to reach the marker the stream is judged on.
///
/// A liveness guard and never a verdict: the boot it waits for prints `Boot:
/// complete` within a second on this host, and what this catches is a guest
/// that stopped talking altogether.
const CEILING: Duration = Duration::from_secs(90);

/// How long the listener is given to see one line after the guest's own console
/// has shown the record that produced it.
///
/// The two channels are different: the console is a 16550 the host reads
/// directly, the stream is a TCP connection through a driver, a stack and
/// slirp. This is the whole of the lag the second is allowed over the first.
const LAG: Duration = Duration::from_secs(20);

/// Which machine the stream is judged on.
///
/// **Two of them, and the driver is the difference** — the same reasoning
/// `common::https` runs on. The stream's whole purpose is a laptop whose NIC is
/// an Intel I219, and QEMU's `e1000e` is the only machine in reach that runs
/// netd's Intel driver at all.
#[derive(Clone, Copy)]
pub struct Bench {
    pub profile: qemu::Profile,
    /// The boot config whose netd claims this machine's card, and whose `logd`
    /// row carries the `netd` connector the stream needs.
    pub config: &'static str,
    /// The `-device` this profile must actually carry, asked of the argv rather
    /// than assumed: a profile with no NIC would make every line below arrive
    /// from nowhere, or not arrive and be believed.
    pub device: &'static str,
}

pub const VIRTIO: Bench =
    Bench { profile: qemu::Profile::Headless, config: "tests/netcase", device: "virtio-net" };

pub const E1000E: Bench =
    Bench { profile: qemu::Profile::E1000e, config: "tests/e1000case", device: "e1000e" };

/// An address on the guest's own network that answers nothing, ever.
///
/// QEMU's user-mode networking hosts four addresses in `10.0.2.0/24` — the
/// gateway, the host, its DNS and the guest — and answers ARP for its own and
/// for nothing else. So a SYN aimed here never leaves the guest's stack: no
/// refusal, no reset, no timeout from a peer, just a connection that is being
/// opened for as long as anyone waits. That is "the cable is out", staged
/// without a cable.
const UNREACHABLE: &str = "10.0.2.99";

/// The port that address does not answer on either. Any number does; a fixed
/// one keeps the parameter line readable.
const UNREACHABLE_PORT: u16 = 41337;

/// A host listener for one boot's records.
///
/// It accepts exactly one connection — a boot has one `logd` — appends every
/// line to a file as it arrives, and keeps them for the comparison at the end.
pub struct Listener {
    /// The host port the guest is told to reach, as `10.0.2.2:<port>`.
    pub port: u16,
    /// Where the lines are appended as they arrive, so a failing run leaves the
    /// stream on disk beside the guest's own log.
    pub path: PathBuf,
    lines: Arc<Mutex<Vec<String>>>,
    connected: Arc<AtomicUsize>,
    ended: Arc<AtomicBool>,
}

impl Listener {
    /// A port nothing is listening on.
    ///
    /// **Bound and released rather than picked out of the air**: a number this
    /// process just held is one the host had free a moment ago. If something
    /// takes it in the meantime the boot connects and the arm reds — a false
    /// red, never a false green, because the line it asserts is the *refusal*.
    pub fn silent_port() -> Result<u16, String> {
        let socket = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .map_err(|e| format!("bind a port to hand back: {e}"))?;
        let port = socket
            .local_addr()
            .map_err(|e| format!("ask a bound socket its port: {e}"))?
            .port();
        drop(socket);
        Ok(port)
    }

    /// A listener reading as fast as the boot writes.
    pub fn start(path: &Path) -> Result<Self, String> {
        let socket = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .map_err(|e| format!("bind the log stream's listener: {e}"))?;
        let port = socket
            .local_addr()
            .map_err(|e| format!("ask the listener its port: {e}"))?
            .port();
        let lines = Arc::new(Mutex::new(Vec::new()));
        let connected = Arc::new(AtomicUsize::new(0));
        let ended = Arc::new(AtomicBool::new(false));
        let file = std::fs::File::create(path)
            .map_err(|e| format!("create {}: {e}", path.display()))?;

        let theirs = (Arc::clone(&lines), Arc::clone(&connected), Arc::clone(&ended));
        std::thread::spawn(move || {
            let (lines, connected, ended) = theirs;
            let Ok((stream, _)) = socket.accept() else { return };
            connected.fetch_add(1, Ordering::SeqCst);
            let mut file = file;
            let mut reader = BufReader::new(stream);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let _ = file.write_all(line.as_bytes());
                        lines.lock().expect("the stream's lines").push(line);
                    }
                }
            }
            let _ = file.flush();
            // **The close is the boot's end**, and it is the one thing this
            // stream says that no line carries: `logd` dies with the machine,
            // netd sees its pipe hang up and closes the socket.
            ended.store(true, Ordering::SeqCst);
        });

        Ok(Self { port, path: path.to_path_buf(), lines, connected, ended })
    }

    pub fn connections(&self) -> usize {
        self.connected.load(Ordering::SeqCst)
    }

    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }

    pub fn lines(&self) -> Vec<String> {
        self.lines.lock().expect("the stream's lines").clone()
    }

    /// Wait for a line carrying `needle`, answering how long it took.
    pub fn wait_for(&self, needle: &str, by: Duration) -> Result<Duration, String> {
        let began = Instant::now();
        while began.elapsed() < by {
            if self.lines().iter().any(|l| l.contains(needle)) {
                return Ok(began.elapsed());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        Err(format!(
            "{needle:?} never arrived on the stream in {by:?}: {} connection(s), {} line(s), \
             ended={}",
            self.connections(),
            self.lines().len(),
            self.ended()
        ))
    }

    /// Wait for the connection to close, which is this boot saying it is over.
    pub fn wait_for_end(&self, by: Duration) -> Result<(), String> {
        let began = Instant::now();
        while began.elapsed() < by {
            if self.ended() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        Err(format!(
            "the stream was still open {by:?} after the guest went down; {} line(s) received",
            self.lines().len()
        ))
    }
}

/// One boot's image, built with the stream's address on it, and where its log
/// partition sits inside it.
struct Staged {
    image: PathBuf,
    start: usize,
    len: usize,
}

fn stage(
    bench: Bench,
    name: &str,
    at: (&'static str, u16),
    extra: &[&str],
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<Staged, String> {
    let config = compile::repo_root().join(bench.config);
    let param = qemu::log_stream_param(at);
    let mut params: Vec<&str> = extra.to_vec();
    params.push(param.as_str());
    let bytes = qemu::build_boot_image(&config, c_bins, rust_bins, &params);
    let image = super::lane::dir().join(format!("{name}.img"));
    std::fs::write(&image, &bytes).map_err(|e| format!("write {}: {e}", image.display()))?;
    let (start, len) = volumes::log_extent(&bytes, &image)?;
    Ok(Staged { image, start, len })
}

/// The `nth` number on a line the guest wrote.
///
/// Untrusted in the sense that matters here: it is what the program under test
/// chose to print, so a line that does not carry the number is a failing
/// verdict rather than a zero.
fn counted(line: &str, nth: usize) -> Result<u64, String> {
    line.split_whitespace()
        .filter_map(|word| word.parse::<u64>().ok())
        .nth(nth)
        .ok_or_else(|| format!("the drop report has no number {}: {line:?}", nth + 1))
}

/// What the guest's own log volume says, read off the device behind the guest's
/// back — the oracle the stream is compared with.
fn on_the_volume(staged: &Staged) -> Result<Vec<String>, String> {
    let (_, bytes) = volumes::newest_log(&staged.image, staged.start, staged.len)?;
    Ok(String::from_utf8_lossy(&bytes).lines().map(|l| format!("{l}\n")).collect())
}

/// The listener received the file's own first lines, in the file's own order,
/// and nothing else.
///
/// The strict reading, for a listener that kept up: `logd` writes a line and
/// then offers it, so with nothing dropped the two are equal up to wherever the
/// connection ended. Reported as the first disagreement rather than as a count —
/// a stream that lost its third line and one that reordered two are different
/// defects, and a length calls them the same one.
fn is_prefix_of(received: &[String], file: &[String]) -> Result<(), String> {
    if received.is_empty() {
        return Err("the listener received nothing at all".to_string());
    }
    for (i, line) in received.iter().enumerate() {
        match file.get(i) {
            Some(theirs) if theirs == line => {}
            Some(theirs) => {
                return Err(format!(
                    "the stream and /log disagree at line {i}:\n  stream: {line:?}\n  /log:   \
                     {theirs:?}"
                ))
            }
            None => {
                return Err(format!(
                    "the stream carries {} line(s) and /log only {}; the first line past the \
                     file is {line:?}",
                    received.len(),
                    file.len()
                ))
            }
        }
    }
    Ok(())
}

/// A boot's records arriving over the wire while it is booting, judged live and
/// then against the file.
pub fn stream(
    bench: Bench,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let name = format!("logstream-{}", bench.device);
    let listener = Listener::start(&super::lane::dir().join(format!("{name}.txt")))?;
    let staged =
        stage(bench, &name, (qemu::GUEST_VIEW_OF_HOST, listener.port), &[], c_bins, rust_bins)?;

    let options = BootOptions {
        profile: bench.profile,
        boot_image: Some(staged.image.clone()),
        log_stream: Some((qemu::GUEST_VIEW_OF_HOST, listener.port)),
        ..Default::default()
    };
    if !qemu::profile_argv(&options).iter().any(|a| a.contains(bench.device)) {
        return Err(format!("this test needs a {} and the profile carries none", bench.device));
    }
    let config = compile::repo_root().join(bench.config);
    let mut guest = QemuInstance::boot_with_options(&config, c_bins, rust_bins, options);
    let mut console = guest.boot_log().to_string();
    serial::Serial::named("boot console", console.as_str()).must_be_clean()?;

    // **The claim is "while it is booting", so it is asserted before anything
    // shuts the guest down.** `Boot: complete` is a kernel record, so its only
    // way here is the ring, `logd`, netd and the wire.
    let took = listener.wait_for("Boot: complete", CEILING)?;
    eprintln!(
        "  [stream] `Boot: complete` reached the host over {} {} ms into the listener's life, \
         with the guest still running",
        bench.device,
        took.as_millis()
    );

    // A job, and its exit record over the wire. The kernel logs `exit:` when
    // the process ends, so this is a record produced *after* the stream was
    // already open — the boot log alone could have been a replay of a ring.
    let job = "test_rs_empty_dir_stat";
    let result = guest.run_test(job, Duration::from_secs(60));
    if result.exit_code != Some(0) {
        return Err(format!("{job} exited {:?}:\n{}", result.exit_code, result.stdout));
    }
    let exit = format!("exit: {job} ");
    let took = listener.wait_for(&exit, LAG)?;
    eprintln!("  [stream] {exit:?} reached the host {} ms after the job ended", took.as_millis());

    // Down, and then the two readings of the same boot.
    writeln!(guest.stdin_mut(), "run shutdown").map_err(|e| format!("write to QEMU stdin: {e}"))?;
    guest.flush_stdin();
    let tail = guest.drain_serial(Duration::from_secs(20));
    console.push_str(&tail);
    drop(guest);
    for bad in ["PANIC:", "panicked at"] {
        if console.contains(bad) {
            return Err(format!("{bad:?} on the way down\n{tail}"));
        }
    }
    listener.wait_for_end(LAG)?;

    let received = listener.lines();
    let file = on_the_volume(&staged)?;
    is_prefix_of(&received, &file)?;
    if received.iter().any(|l| l.contains("never reached the log stream")) {
        return Err(format!(
            "a listener that read every line was still reported as behind this machine: {:?}",
            received.iter().find(|l| l.contains("never reached the log stream"))
        ));
    }
    // Non-vacuity: a comparison over three lines proves nothing about a boot.
    if received.len() < 100 {
        return Err(format!(
            "the stream carried {} line(s), which is fewer than a boot writes",
            received.len()
        ));
    }
    eprintln!(
        "  [stream] {} line(s) over the wire, every one of them the same line /log holds ({} \
         line(s) in the file); the connection closed with the machine",
        received.len(),
        file.len()
    );
    let _ = std::fs::remove_file(&staged.image);
    Ok(())
}

/// A boot told to stream to a port nothing is listening on: the file is whole
/// and carries the one line saying what could not be done.
pub fn no_listener(
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let bench = VIRTIO;
    let port = Listener::silent_port()?;
    let staged =
        stage(bench, "logstream-silent", (qemu::GUEST_VIEW_OF_HOST, port), &[], c_bins, rust_bins)?;

    let options = BootOptions {
        profile: bench.profile,
        boot_image: Some(staged.image.clone()),
        log_stream: Some((qemu::GUEST_VIEW_OF_HOST, port)),
        ..Default::default()
    };
    let config = compile::repo_root().join(bench.config);
    let mut guest = QemuInstance::boot_with_options(&config, c_bins, rust_bins, options);
    let mut console = guest.boot_log().to_string();
    serial::Serial::named("boot console", console.as_str()).must_be_clean()?;

    // The boot goes on. That is the claim: a stream nobody is listening to
    // costs this machine its stream and nothing else.
    let result = guest.run_test("test_rs_empty_dir_stat", Duration::from_secs(60));
    if result.exit_code != Some(0) {
        return Err(format!(
            "a boot whose log stream found no listener could not run a job: {:?}\n{}",
            result.exit_code, result.stdout
        ));
    }
    writeln!(guest.stdin_mut(), "run shutdown").map_err(|e| format!("write to QEMU stdin: {e}"))?;
    guest.flush_stdin();
    console.push_str(&guest.drain_serial(Duration::from_secs(20)));
    drop(guest);

    let file = on_the_volume(&staged)?;
    let refusals: Vec<&String> =
        file.iter().filter(|l| l.contains("for this boot's log stream")).collect();
    let [said] = refusals.as_slice() else {
        return Err(format!(
            "the file carries {} line(s) about a log stream that never opened, and it owes \
             exactly one; the file has {} line(s), ending {:?}",
            refusals.len(),
            file.len(),
            file.iter().rev().take(3).collect::<Vec<_>>()
        ));
    };
    if !said.contains(&format!("{}:{port}", qemu::GUEST_VIEW_OF_HOST)) {
        return Err(format!("the refusal does not say which address it was: {said:?}"));
    }
    if !file.iter().any(|l| l.contains("Boot: complete")) {
        return Err("the file stops before `Boot: complete`, so the stream cost it records"
            .to_string());
    }
    eprintln!("  [stream] no listener, and the file says so once: {}", said.trim_end());
    let _ = std::fs::remove_file(&staged.image);
    Ok(())
}

/// A boot whose stream address is on this machine's network and answers
/// nothing: every line offered while the connection is being opened is refused
/// by the queue, counted, and reported — and `/log` is whole regardless.
///
/// **This is the arm the queue's bound exists for, and staging it took two
/// tries.** The first stalled a real listener on the host and measured nothing:
/// a stalled peer's backpressure has to travel through a 2 MiB kernel pipe
/// (`kernel/src/pipe.rs`'s `PIPE_SIZE`) and netd's own 64 KiB send buffer before
/// it reaches this queue at all, and a `log-storm` at `--smp 8` produced 4,213
/// lines — 674 KiB, measured — which every one of those buffers swallowed with
/// room to spare. A peer that never answers has no such path: nothing drains,
/// so the queue is the first thing to fill and the only thing that can refuse.
pub fn unreachable(
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let bench = VIRTIO;
    // `log-storm` is what makes this machine produce records faster than a
    // stream that is going nowhere can take them; the arm is baked into the
    // image, which is what `BootOptions::kernel_params` would otherwise only
    // have looked like it did.
    let staged = stage(
        bench,
        "logstream-unreachable",
        (UNREACHABLE, UNREACHABLE_PORT),
        &["log-storm"],
        c_bins,
        rust_bins,
    )?;

    let options = BootOptions {
        profile: bench.profile,
        boot_image: Some(staged.image.clone()),
        log_stream: Some((UNREACHABLE, UNREACHABLE_PORT)),
        kernel_params: &["log-storm"],
        smp: 8,
        ..Default::default()
    };
    let config = compile::repo_root().join(bench.config);
    let mut guest = QemuInstance::boot_with_options(&config, c_bins, rust_bins, options);
    let mut console = guest.boot_log().to_string();

    // The storm starts on the first `SYS_LOG_READ`, which `logd` makes before
    // this line is printed; what it produces has nowhere to go.
    qemu::await_marker(&mut guest, &mut console, "logstorm done t=", "the storm to run out")?;

    // **The guest goes on working.** That is the claim the whole design turns
    // on: a stream that cannot deliver a byte costs this machine its stream and
    // nothing else.
    let result = guest.run_test("test_rs_empty_dir_stat", Duration::from_secs(60));
    if result.exit_code != Some(0) {
        return Err(format!(
            "a boot whose log stream reached nothing could not run a job: {:?}\n{}",
            result.exit_code, result.stdout
        ));
    }
    writeln!(guest.stdin_mut(), "run shutdown").map_err(|e| format!("write to QEMU stdin: {e}"))?;
    guest.flush_stdin();
    console.push_str(&guest.drain_serial(Duration::from_secs(20)));
    drop(guest);

    let file = on_the_volume(&staged)?;
    let reports: Vec<&String> =
        file.iter().filter(|l| l.contains("never reached the log stream")).collect();
    if reports.is_empty() {
        return Err(format!(
            "a log storm offered to a stream that reaches nothing cost it no record it admits \
             to; the file has {} line(s), ending {:?}",
            file.len(),
            file.iter().rev().take(3).collect::<Vec<_>>()
        ));
    }
    // **The accounting is checked against itself.** Every report says what its
    // own run of loss added and what the boot has lost in total, so the first
    // numbers must sum to the last line's second one. A counter that has stopped
    // counting cannot satisfy both, and a report nobody can read is the same as
    // no report at all.
    let mut added = 0u64;
    for line in &reports {
        added += counted(line, 0)?;
    }
    let last = reports.last().expect("a non-empty list has a last");
    let dropped = counted(last, 1)?;
    if dropped == 0 {
        return Err(format!("the stream reported dropping nothing, in {last:?}"));
    }
    if added != dropped {
        return Err(format!(
            "the drop reports add up to {added} and the last one says {dropped} for the whole \
             boot, so what is counted is not the drops:\n{}",
            reports.iter().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n")
        ));
    }

    // **The file goes on being written on the far side of every drop.** That is
    // what the stream may never cost, and a storm with nowhere to send it is
    // exactly when it would. `Boot: complete` is not the marker to ask for
    // here: the storm overruns the kernel's own record ring, so an early
    // record's absence is the ring's doing and says nothing about the stream —
    // the job's exit record is, because it happened after the queue had already
    // started refusing lines.
    let owed = "exit: test_rs_empty_dir_stat ";
    if !file.iter().any(|l| l.contains(owed)) {
        return Err(format!(
            "{owed:?} never reached /log on a boot whose stream went nowhere; the file has {} \
             line(s), ending {:?}",
            file.len(),
            file.iter().rev().take(3).collect::<Vec<_>>()
        ));
    }
    let stormed = file.iter().filter(|l| l.contains("logstorm t=")).count();
    if stormed == 0 {
        return Err("the storm reached the log stream's arm and not the file".to_string());
    }
    eprintln!(
        "  [stream] {stormed} storm record(s) in /log and a stream that reached nothing; \
         {dropped} line(s) refused by the queue and the log says so in {} line(s)",
        reports.len()
    );
    let _ = std::fs::remove_file(&staged.image);
    Ok(())
}
