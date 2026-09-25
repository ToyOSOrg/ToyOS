//! The log a machine serves, judged from the host: a guest whose `logd` port
//! is forwarded, a reader that connects when the test says so, and the guest's
//! own `/log` as the oracle.
//!
//! **The file is what the stream is judged against.** `logd` writes each round
//! to `/log` and then hands the same bytes to every reader, from the boot's
//! first line however late the reader came, so what a reader received is the
//! file's own first lines in the file's own order ([`is_prefix_of`]). The file
//! is read off the FAT volume behind the guest's back, so the two readings
//! share nothing but the boot that produced them.

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use toyos_build::metaltalk::{Peer, Stream};

use super::qemu::{self, BootOptions, QemuInstance};
use super::{compile, serial, volumes};

/// A liveness guard on a guest that stopped talking, never a verdict.
const CEILING: Duration = Duration::from_secs(90);

/// What `logd` says once its port is open: the moment a reader can connect.
pub const SERVING: &str = "logd: serving this boot's log on port";

/// Which machine the stream is judged on. **Two of them, and the driver is the
/// difference** — the bench's NIC is an Intel I219, and QEMU's `e1000e` is the
/// only machine in reach that runs netd's Intel driver.
#[derive(Clone, Copy)]
pub struct Bench {
    pub profile: qemu::Profile,
    /// The boot config whose netd claims this machine's card, and whose `logd`
    /// row carries the `netd` connector serving needs.
    pub config: &'static str,
    /// The `-device` this profile must actually carry, asked of the argv rather
    /// than assumed.
    pub device: &'static str,
}

pub const VIRTIO: Bench =
    Bench { profile: qemu::Profile::Headless, config: "tests/netcase", device: "virtio-net" };

pub const E1000E: Bench =
    Bench { profile: qemu::Profile::E1000e, config: "tests/e1000case", device: "e1000e" };

/// One boot's image and where its log partition sits inside it.
pub struct Staged {
    pub image: PathBuf,
    pub start: usize,
    pub len: usize,
}

pub fn stage(
    config: &str,
    name: &str,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<Staged, String> {
    let config = compile::repo_root().join(config);
    let bytes = qemu::build_boot_image(&config, c_bins, rust_bins, &[]);
    let image = super::lane::dir().join(format!("{name}.img"));
    std::fs::write(&image, &bytes).map_err(|e| format!("write {}: {e}", image.display()))?;
    let (start, len) = volumes::log_extent(&bytes, &image)?;
    Ok(Staged { image, start, len })
}

/// A boot of `bench` with `logd`'s port forwarded to `port`, up and serving.
fn boot(
    bench: Bench,
    staged: &Staged,
    port: u16,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(QemuInstance, String), String> {
    let options = BootOptions {
        profile: bench.profile,
        boot_image: Some(qemu::Staged::Written(staged.image.clone())),
        log_port: Some(port),
        ..Default::default()
    };
    if !qemu::profile_argv(&options).iter().any(|a| a.contains(bench.device)) {
        return Err(format!("this test needs a {} and the profile carries none", bench.device));
    }
    let config = compile::repo_root().join(bench.config);
    let mut guest = QemuInstance::boot_with_options(&config, c_bins, rust_bins, options);
    let mut console = guest.boot_log().to_string();
    qemu::await_marker(&mut guest, &mut console, SERVING, "logd to open its port")?;
    serial::Serial::named("boot console", console.as_str()).must_be_clean()?;
    Ok((guest, console))
}

/// A reader of the forwarded port, connected now.
pub fn reader(port: u16, file: &str) -> Result<Stream, String> {
    let at = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let stream = Stream::connect(Peer::At(at), &super::lane::dir().join(file), false, CEILING)?;
    stream
        .wait_connected(CEILING)
        .ok_or_else(|| stream.unopened().unwrap_or_else(|| "the stream never opened".to_string()))?;
    Ok(stream)
}

/// Shut the guest down, wait for QEMU to exit, and read what it left on its
/// volume.
pub fn shut_down(guest: QemuInstance, console: &mut String, staged: &Staged) -> Result<Vec<String>, String> {
    shut_down_keeping(guest, console, staged, |_| ()).map(|(file, ())| file)
}

/// [`shut_down`], with `keep` handed the guest once QEMU has exited and before
/// it is dropped: what QEMU finishes only at its exit — the wav it captured —
/// is whole then, and gone once the guest is dropped.
pub fn shut_down_keeping<R>(
    mut guest: QemuInstance,
    console: &mut String,
    staged: &Staged,
    keep: impl FnOnce(&QemuInstance) -> R,
) -> Result<(Vec<String>, R), String> {
    writeln!(guest.stdin_mut(), "run shutdown").map_err(|e| format!("write to QEMU stdin: {e}"))?;
    guest.flush_stdin();
    console.push_str(&guest.await_exit(Duration::from_secs(20))?);
    let kept = keep(&guest);
    drop(guest);
    for bad in ["PANIC:", "panicked at"] {
        if console.contains(bad) {
            return Err(format!("{bad:?} on the way down\n{console}"));
        }
    }
    Ok((volumes::whole_log(&staged.image, staged.start, staged.len)?, kept))
}

/// What a reader received is the file's own first lines, in the file's own
/// order, and nothing else.
///
/// Reported as the first disagreement rather than as a count: a stream that lost
/// its third line and one that reordered two are different defects, and a length
/// calls them the same one.
pub fn is_prefix_of(received: &[String], file: &[String]) -> Result<(), String> {
    if received.is_empty() {
        return Err("the reader received nothing at all".to_string());
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

/// **A reader that connects late gets the whole boot.** A job runs and ends
/// before anything connects; then a reader connects and must receive the boot
/// from its first line — the job's own line and the kernel's record of its exit
/// among it — and then what the machine writes after, all of it the same lines
/// `/log` holds, in its order.
pub fn stream(
    bench: Bench,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let name = format!("logstream-{}", bench.device);
    let staged = stage(bench.config, &name, c_bins, rust_bins)?;
    let port = qemu::free_host_port();
    let (mut guest, mut console) = boot(bench, &staged, port, c_bins, rust_bins)?;

    // Before any reader exists.
    let job = "test_rs_log_origin";
    let before = guest.run_test(job, Duration::from_secs(60));
    if before.exit_code != Some(0) {
        return Err(format!("{job} exited {:?}:\n{}", before.exit_code, before.stdout));
    }
    let stream = reader(port, &format!("{name}.txt"))?;
    let exit = format!("exit: {job} ");
    if !stream.wait_for(&exit, CEILING) {
        return Err(format!(
            "a reader that connected after {job} ended never received its exit record: {} \
             line(s)",
            stream.lines().len()
        ));
    }
    // And after: a record written once the reader was already reading.
    let later = "test_rs_empty_dir_stat";
    let after = guest.run_test(later, Duration::from_secs(60));
    if after.exit_code != Some(0) {
        return Err(format!("{later} exited {:?}:\n{}", after.exit_code, after.stdout));
    }
    if !stream.wait_for(&format!("exit: {later} "), CEILING) {
        return Err("a record written while the reader read never reached it".to_string());
    }

    let file = shut_down(guest, &mut console, &staged)?;
    if !stream.wait_ended(CEILING) {
        return Err("the reader's connection had not ended once the guest was down".to_string());
    }
    let received = stream.lines();
    is_prefix_of(&received, &file)?;
    let whole = received.concat();
    if toyos_build::bootlog::boot_millis(&whole).is_none() {
        return Err("the reader was not handed this boot's `Boot: complete`".to_string());
    }
    if !toyos_build::bootlog::lines_of(&whole, "test-runner").contains(super::origin::NONCE) {
        return Err(format!("the reader was not handed {job}'s own line, said before it connected"));
    }
    eprintln!(
        "  [stream] a reader that connected after the first job ended got {} line(s) over {}, \
         from the boot's first, each the line /log holds ({} in the file)",
        received.len(),
        bench.device,
        file.len()
    );
    let _ = std::fs::remove_file(&staged.image);
    Ok(())
}

/// `logd`'s readers on the network at once (`serve.rs`'s `MAX_NETWORK_READERS`).
const NETWORK_READERS: usize = 8;

/// A liveness guard on the flood reaching a reader: five megabytes through a
/// TCG guest's netd, as long as the flood job itself is given.
const FLOOD_CEILING: Duration = Duration::from_secs(300);

/// How long `logd` lets a reader take no bytes before it lets it go
/// (`serve.rs`'s `STALLED`).
const STALLED_SECS: u64 = 10;

/// What `logd` says as it lets a reader go that took no bytes it was owed.
const LET_GO: &str = "logd: letting ";

/// **A reader that stops reading costs nobody else anything, and its slot is
/// not kept.** Every network slot `logd` has is taken by a connection that
/// never reads, while a program floods its output past every buffer between
/// them; the file takes every line, `logd` lets each stalled reader go, and a
/// reader that connects after that is handed the whole boot, the flood's last
/// line included.
pub fn stalled_reader(
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let bench = VIRTIO;
    let staged = stage(bench.config, "logstream-stalled", c_bins, rust_bins)?;
    let port = qemu::free_host_port();
    let (mut guest, mut console) = boot(bench, &staged, port, c_bins, rust_bins)?;

    let stalled = (0..NETWORK_READERS)
        .map(|_| never_read(port))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("connect the readers that will not read: {e}"))?;
    let from = console.len();
    let flood = guest.run_test(super::origin::FLOODER, Duration::from_secs(300));
    if flood.exit_code != Some(0) {
        return Err(format!("the flood exited {:?}", flood.exit_code));
    }
    console.push_str(&flood.before);
    console.push_str(&flood.serial);
    // Every stalled reader's writes stopped being taken during the flood, whose
    // five megabytes outrun every buffer between it and logd, so each let-go is
    // owed `STALLED_SECS` after the flood's end at the latest. Twice that,
    // widened by this host, is logd's promise judged; a guest that stays quiet
    // past it has broken the promise, which is this test's verdict and not a
    // stall of the harness.
    let flood_ended = Instant::now();
    let deadline = flood_ended + guest.budget(Duration::from_secs(2 * STALLED_SECS));
    let seen = |console: &str| console[from.min(console.len())..].matches(LET_GO).count();
    while seen(&console) < NETWORK_READERS {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let more = guest.drain_until(left, |line| line.contains(LET_GO));
        console.push_str(&more);
    }
    let let_go = seen(&console);
    if let_go < NETWORK_READERS {
        let waited = flood_ended.elapsed().as_secs();
        let said = match shut_down(guest, &mut console, &staged) {
            Ok(file) => file.iter().filter(|l| l.contains("logd: ")).cloned().collect::<String>(),
            Err(why) => format!("none read: {}", why.lines().next().unwrap_or("")),
        };
        return Err(format!(
            "logd let {let_go} of the {NETWORK_READERS} readers that stopped reading go in the {waited} s \
             after the flood ended, and owes each one {STALLED_SECS} s after its writes stop being \
             taken; /log's logd lines:\n{said}"
        ));
    }
    let second = reader(port, "logstream-stalled-second.txt")?;
    if !second.wait_for(super::origin::FLOOD_DONE, FLOOD_CEILING) {
        return Err(format!(
            "a reader that connected after the flood, once every stalled reader was let go, did \
             not receive the flood's last line: {} line(s)",
            second.lines().len()
        ));
    }
    let file = shut_down(guest, &mut console, &staged)?;
    drop(stalled);
    if !second.wait_ended(CEILING) {
        return Err("the reader's connection had not ended once the guest was down".to_string());
    }
    let received = second.lines();
    is_prefix_of(&received, &file)?;
    let floods = received.iter().filter(|l| l.contains("} flood ")).count();
    let let_go = file.iter().filter(|l| l.contains(LET_GO)).count();
    eprintln!(
        "  [stream] {let_go} reader(s) that never read were let go; a reader after them got {} \
         line(s), {floods} of them the flood's, each the line /log holds",
        received.len()
    );
    let _ = std::fs::remove_file(&staged.image);
    Ok(())
}

/// The receive buffer a reader that never reads is given: set before the
/// connect, so this host's autotuning does not grow it, and the window it
/// advertises closes once this much has arrived.
const NARROW_WINDOW: libc::c_int = 16 * 1024;

/// A connection to this host's `port`, admitted — its first line read — and
/// never read again, with a receive buffer of [`NARROW_WINDOW`]: the peer a
/// zero window makes of it.
fn never_read(port: u16) -> Result<TcpStream, String> {
    use std::os::fd::FromRawFd;
    let failed = |what: &str| format!("{what}: {}", std::io::Error::last_os_error());
    // SAFETY: a fresh descriptor, owned by the `TcpStream` made of it at once
    // so every path below closes it.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(failed("socket"));
    }
    // SAFETY: `fd` is the descriptor just made, and nothing else owns it.
    let stream = unsafe { TcpStream::from_raw_fd(fd) };
    let size = NARROW_WINDOW;
    // SAFETY: `size` outlives the call, and the length is its own.
    let set = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            (&size as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if set != 0 {
        return Err(failed("SO_RCVBUF"));
    }
    // SAFETY: all-zero is a valid `sockaddr_in`, whose fields are integers.
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    #[cfg(target_os = "macos")]
    {
        addr.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
    }
    addr.sin_family = libc::AF_INET as libc::sa_family_t;
    addr.sin_port = port.to_be();
    addr.sin_addr = libc::in_addr { s_addr: u32::from(Ipv4Addr::LOCALHOST).to_be() };
    // SAFETY: `addr` is a whole `sockaddr_in` and the length says so.
    let connected = unsafe {
        libc::connect(
            fd,
            (&addr as *const libc::sockaddr_in).cast(),
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if connected != 0 {
        return Err(failed("connect"));
    }
    // Admitted once it carries a line: `logd` hands every reader the boot's
    // first line at once. One byte at a time, so nothing past it is taken.
    use std::io::Read;
    let mut stream = stream;
    stream.set_read_timeout(Some(CEILING)).map_err(|e| format!("a read bound: {e}"))?;
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(1) if byte[0] == b'\n' => break,
            Ok(1) => {}
            other => return Err(format!("a reader that will not read was never admitted: {other:?}")),
        }
    }
    stream.set_read_timeout(None).map_err(|e| format!("a read bound: {e}"))?;
    Ok(stream)
}
