//! The record stream's judge: a listener on the host, a guest booted with its
//! address on the parameter line, and the guest's own `/log` as the oracle.
//!
//! **The file is what the stream is judged against.** `logd` writes a line to
//! `/log` and then offers the same line to the stream, so a listener that kept
//! up received that file's own first lines and nothing else ([`is_prefix_of`]).
//! The file is read off the FAT volume behind the guest's back, so the two
//! readings share nothing but the boot that produced them.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::qemu::{self, BootOptions, QemuInstance};
use super::{compile, serial, volumes};

/// A liveness guard on a guest that stopped talking, never a verdict.
const CEILING: Duration = Duration::from_secs(90);

/// The whole of the lag the stream is allowed over the guest's own console,
/// which the host reads off a 16550 while the stream crosses a driver, a stack
/// and slirp.
const LAG: Duration = Duration::from_secs(20);

/// Which machine the stream is judged on. **Two of them, and the driver is the
/// difference** — the bench's NIC is an Intel I219, and QEMU's `e1000e` is the
/// only machine in reach that runs netd's Intel driver.
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

/// What a host holds of a stream whose reader has stopped taking it.
///
/// Held rather than left to the runner, so a peer that stops reading closes its
/// window promptly on every host — which is what puts netd's send buffer under
/// its own bound and the writer inside a blocking write. **It does not bound
/// what the machine absorbs**: the pipe below netd is megabytes and what QEMU's
/// user-mode networking holds is nothing this tree names, so how much a stalled
/// peer costs is not a number any arm here may assert.
const STALLED_PEER_WINDOW: usize = 32 * 1024;

/// An address on the guest's own network that answers nothing, ever.
///
/// QEMU's user-mode networking answers ARP for the four addresses it hosts in
/// `10.0.2.0/24` and for nothing else, so a SYN aimed here never leaves the
/// guest's stack: no refusal, no reset, no peer. That is "the cable is out",
/// staged without a cable.
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
    /// Cleared by [`Listener::stalled`]: the thread accepts the connection and
    /// then reads nothing until [`Listener::release`] sets it.
    reading: Arc<AtomicBool>,
}

impl Listener {
    /// A port nothing is listening on: bound and released rather than picked
    /// out of the air. Something taking it in the meantime is a false red and
    /// never a false green, because the line the arm asserts is the *refusal*.
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
        Self::bound(path, true)
    }

    /// A listener that accepts the connection and then reads nothing, so the
    /// guest's own buffers are what the boot's records pile up in.
    ///
    /// It is released and drained later, because a peer that never reads says
    /// nothing about what reached it: what arrived is the evidence the writer
    /// got as far as writing at all.
    pub fn stalled(path: &Path) -> Result<Self, String> {
        Self::bound(path, false)
    }

    /// Read whatever the peer piled up while this listener was stalled.
    pub fn release(&self) {
        self.reading.store(true, Ordering::SeqCst);
    }

    fn bound(path: &Path, reading: bool) -> Result<Self, String> {
        let socket = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .map_err(|e| format!("bind the log stream's listener: {e}"))?;
        if !reading {
            clamp_receive_buffer(&socket, STALLED_PEER_WINDOW)?;
        }
        let port = socket
            .local_addr()
            .map_err(|e| format!("ask the listener its port: {e}"))?
            .port();
        let lines = Arc::new(Mutex::new(Vec::new()));
        let connected = Arc::new(AtomicUsize::new(0));
        let ended = Arc::new(AtomicBool::new(false));
        let reading = Arc::new(AtomicBool::new(reading));
        let file = std::fs::File::create(path)
            .map_err(|e| format!("create {}: {e}", path.display()))?;

        let theirs = (
            Arc::clone(&lines),
            Arc::clone(&connected),
            Arc::clone(&ended),
            Arc::clone(&reading),
        );
        std::thread::spawn(move || {
            let (lines, connected, ended, reading) = theirs;
            let Ok((stream, _)) = socket.accept() else { return };
            connected.fetch_add(1, Ordering::SeqCst);
            while !reading.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50));
            }
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

        Ok(Self { port, path: path.to_path_buf(), lines, connected, ended, reading })
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

    /// Wait until nothing new has arrived for `still`, answering how many lines
    /// have.
    ///
    /// A liveness guard on a drain no line announces: what it waits for is the
    /// backlog a released peer takes, and a guest that is merely idle goes quiet
    /// long before `still`.
    pub fn wait_until_quiet(&self, still: Duration, by: Duration) -> Result<usize, String> {
        let began = Instant::now();
        let mut seen = self.lines().len();
        let mut since = Instant::now();
        while began.elapsed() < by {
            std::thread::sleep(Duration::from_millis(100));
            let now = self.lines().len();
            if now != seen {
                seen = now;
                since = Instant::now();
            } else if since.elapsed() >= still {
                return Ok(seen);
            }
        }
        Err(format!(
            "the stream was still arriving {by:?} after the peer read again; {seen} line(s) so far"
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

/// Hold this socket's receive buffer to `bytes`, so what a peer that stops
/// reading can absorb is this test's number and not the runner's.
fn clamp_receive_buffer(socket: &TcpListener, bytes: usize) -> Result<(), String> {
    let size = bytes as libc::c_int;
    // SAFETY: `socket` owns the descriptor for the whole call, and the pointer
    // and length describe the one `c_int` `SO_RCVBUF` is documented to take.
    let set = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            std::ptr::addr_of!(size).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if set != 0 {
        return Err(format!(
            "hold the listener's receive buffer to {bytes} bytes: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
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

/// The two numbers a drop report carries: what this run of loss added, and what
/// the boot has lost in total.
///
/// Anchored on the report's own words and not on a position, so a line that
/// grows a prefix still reads. A line that carries neither is a failing verdict
/// rather than a zero.
fn drop_report(line: &str) -> Result<(u64, u64), String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let after = |word: &str| -> Option<u64> {
        let at = words.iter().position(|w| *w == word)?;
        words.get(at + 1)?.parse().ok()
    };
    match (after("logd:"), after("and")) {
        (Some(run), Some(total)) => Ok((run, total)),
        _ => Err(format!("this line is not a drop report: {line:?}")),
    }
}

/// What the guest's own log volume says, read off the device behind the guest's
/// back — the oracle the stream is compared with.
///
/// Every file this boot wrote, because a boot that logs enough to starve a
/// stream logs enough to rotate, and a rotation is not a hole.
fn on_the_volume(staged: &Staged) -> Result<Vec<String>, String> {
    volumes::whole_log(&staged.image, staged.start, staged.len)
}

/// What this boot's own log says the stream refused, and in how many lines, or
/// `None` when it says it refused nothing.
///
/// **The accounting is checked against itself.** Every report says what its own
/// run of loss added and what the boot has lost in total, so the first numbers
/// must sum to the last line's second one. A counter that has stopped counting
/// cannot satisfy both, and a report nobody can read is the same as no report.
///
/// Whether a boot refuses anything at all is a fact about buffers no arm here
/// owns; whether what it says about its refusals holds together is not.
fn refusals_in(file: &[String]) -> Result<Option<(u64, usize)>, String> {
    let reports: Vec<&String> =
        file.iter().filter(|l| l.contains("never reached the log stream")).collect();
    let Some(last) = reports.last() else {
        return Ok(None);
    };
    let mut added = 0u64;
    for line in &reports {
        added += drop_report(line)?.0;
    }
    let dropped = drop_report(last)?.1;
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
    Ok(Some((dropped, reports.len())))
}

/// The same, where refusing nothing is a failing verdict: a stream whose address
/// answers nothing drains never, so a boot that offered a storm to it and lost
/// no line measured nothing at all.
fn refused_in(file: &[String]) -> Result<(u64, usize), String> {
    refusals_in(file)?.ok_or_else(|| {
        format!(
            "a log storm offered to a stream that could not take it cost it no record it admits \
             to; the file has {} line(s), ending {:?}",
            file.len(),
            file.iter().rev().take(3).collect::<Vec<_>>()
        )
    })
}

/// What the listener received is the file's own lines in the file's own order,
/// with holes where the queue refused one.
///
/// A peer that stalls costs the stream lines; it may not reorder, duplicate,
/// invent or cut one. **Every received line is a whole record, including the
/// last** — a line short of the file's is a byte the stream consumed and did
/// not deliver, which is the failure this comparison exists to catch, and no
/// position in the stream is a place it may happen.
fn is_subsequence_of(received: &[String], file: &[String]) -> Result<(), String> {
    let mut at = 0usize;
    for line in received {
        match file[at..].iter().position(|theirs| theirs == line) {
            Some(step) => at += step + 1,
            None => {
                return Err(format!(
                    "the stream carries {line:?}, which /log does not carry after its line {at}"
                ))
            }
        }
    }
    Ok(())
}

/// The listener received the file's own first lines, in the file's own order,
/// and nothing else.
///
/// Reported as the first disagreement rather than as a count: a stream that lost
/// its third line and one that reordered two are different defects, and a length
/// calls them the same one.
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
        boot_image: Some(qemu::Staged::Written(staged.image.clone())),
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
        boot_image: Some(qemu::Staged::Written(staged.image.clone())),
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
/// **A peer that never answers is where the queue is the first buffer to
/// fill**: nothing below it drains, so it is the only thing that can refuse.
pub fn unreachable(
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let bench = VIRTIO;
    // `log-storm` is what makes this machine produce records faster than a
    // stream that is going nowhere can take them, and it is baked into the
    // image rather than passed to a staged one.
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
        boot_image: Some(qemu::Staged::Written(staged.image.clone())),
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
    let (dropped, said_in) = refused_in(&file)?;

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
         {dropped} line(s) refused by the queue and the log says so in {said_in} line(s)"
    );
    let _ = std::fs::remove_file(&staged.image);
    Ok(())
}

/// A peer that accepts the connection, stops reading for a storm's worth of
/// records, and then reads again: every line it receives is a whole record, and
/// `/log`'s own, in `/log`'s own order.
///
/// **That is what a stall may cost and what it may not.** It may cost lines —
/// how many is the pipe's size, netd's, and whatever QEMU's user-mode
/// networking holds between them, none of which this arm owns, so it demands no
/// refusal. It may never cost half a line: a byte the machine consumed and did
/// not deliver is a record cut on the wire, which is what netd discarding the
/// tail of a short send produces and what [`is_subsequence_of`] refuses at every
/// position.
///
/// **The peer is released while the guest is still running**, which is what
/// puts any such cut in the middle of a stream that goes on past it. Ending the
/// arm at the stall instead leaves every cut on the last line, where a
/// comparison can no longer tell a truncation from the connection's own end.
///
/// What a boot *says* it refused is checked against itself where it says
/// anything ([`refusals_in`]); that the accounting counts what it refuses at all
/// is `toyos-logstream`'s host tests, which need no guest and no host's buffers.
pub fn stalled_peer(
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let bench = VIRTIO;
    let storm: &[&str] = &["log-storm", "log-storm-wide"];
    let listener = Listener::stalled(&super::lane::dir().join("logstream-stalled.txt"))?;
    let staged = stage(
        bench,
        "logstream-stalled",
        (qemu::GUEST_VIEW_OF_HOST, listener.port),
        storm,
        c_bins,
        rust_bins,
    )?;

    let options = BootOptions {
        profile: bench.profile,
        boot_image: Some(qemu::Staged::Written(staged.image.clone())),
        log_stream: Some((qemu::GUEST_VIEW_OF_HOST, listener.port)),
        kernel_params: storm,
        smp: 8,
        ..Default::default()
    };
    let config = compile::repo_root().join(bench.config);
    let mut guest = QemuInstance::boot_with_options(&config, c_bins, rust_bins, options);
    let mut console = guest.boot_log().to_string();

    qemu::await_marker(&mut guest, &mut console, "logstorm done t=", "the storm to run out")?;

    // **The guest goes on working**, which is the claim a peer that stopped
    // reading tests and a peer that never answered does not: this one has the
    // machine's own writer blocked on it.
    let result = guest.run_test("test_rs_empty_dir_stat", Duration::from_secs(60));
    if result.exit_code != Some(0) {
        return Err(format!(
            "a boot whose log stream stalled could not run a job: {:?}\n{}",
            result.exit_code, result.stdout
        ));
    }

    // The peer reads again, and the machine stays up until the backlog it was
    // holding has gone past. Everything the stall cost is then in the middle of
    // the stream rather than at its end.
    listener.release();
    listener.wait_until_quiet(Duration::from_secs(2), LAG)?;

    writeln!(guest.stdin_mut(), "run shutdown").map_err(|e| format!("write to QEMU stdin: {e}"))?;
    guest.flush_stdin();
    console.push_str(&guest.drain_serial(Duration::from_secs(20)));
    drop(guest);
    listener.wait_for_end(LAG)?;

    // The connection was opened, so the writer entered the loop this arm is
    // about rather than giving up in `open`.
    if listener.connections() != 1 {
        return Err(format!(
            "the stream opened {} time(s), so nothing was ever written into it",
            listener.connections()
        ));
    }
    let file = on_the_volume(&staged)?;
    // **Whether this host's buffers made the machine refuse anything is not
    // asserted** — how much a stalled peer costs is the pipe's size, netd's,
    // and what QEMU holds between them, and two hosted runs proved that is not
    // a number this arm may demand. What it does assert is that whatever the
    // boot says about its refusals holds together. The accounting itself is
    // `toyos-logstream`'s host tests, which red under the drop-count mutation.
    let refused = refusals_in(&file)?;

    let received = listener.lines();
    let bytes: usize = received.iter().map(String::len).sum();
    if bytes <= toyos_logstream::MAX_BACKLOG_BYTES {
        return Err(format!(
            "the peer received {bytes} byte(s), which the queue alone holds ({}), so nothing \
             here says a stream went through the writer at all",
            toyos_logstream::MAX_BACKLOG_BYTES
        ));
    }
    is_subsequence_of(&received, &file)?;

    let owed = "exit: test_rs_empty_dir_stat ";
    if !file.iter().any(|l| l.contains(owed)) {
        return Err(format!(
            "{owed:?} never reached /log on a boot whose stream stalled; the file has {} line(s), \
             ending {:?}",
            file.len(),
            file.iter().rev().take(3).collect::<Vec<_>>()
        ));
    }
    eprintln!(
        "  [stream] a peer that stopped reading and then read again took {bytes} byte(s) in {} \
         whole line(s), each /log's own in /log's own order; /log holds {} line(s) and {}",
        received.len(),
        file.len(),
        match refused {
            Some((dropped, said_in)) =>
                format!("says it refused {dropped} of them in {said_in} line(s) that agree"),
            None => "says it refused none".to_string(),
        }
    );
    let _ = std::fs::remove_file(&staged.image);
    Ok(())
}
