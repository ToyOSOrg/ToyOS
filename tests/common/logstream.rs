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
use std::time::Duration;

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

/// Shut the guest down and read what it left on its volume.
pub fn shut_down(
    mut guest: QemuInstance,
    console: &mut String,
    staged: &Staged,
) -> Result<Vec<String>, String> {
    writeln!(guest.stdin_mut(), "run shutdown").map_err(|e| format!("write to QEMU stdin: {e}"))?;
    guest.flush_stdin();
    console.push_str(&guest.drain_serial(Duration::from_secs(20)));
    drop(guest);
    for bad in ["PANIC:", "panicked at"] {
        if console.contains(bad) {
            return Err(format!("{bad:?} on the way down\n{console}"));
        }
    }
    volumes::whole_log(&staged.image, staged.start, staged.len)
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
    stream.wait_ended(CEILING);
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

/// **A reader that stops reading costs nobody else anything.** One reader
/// connects and never reads while a program floods its output past every
/// buffer between the two; the file takes every line, and a second reader that
/// connects after the flood is handed the whole boot, the flood's last line
/// included.
pub fn stalled_reader(
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let bench = VIRTIO;
    let staged = stage(bench.config, "logstream-stalled", c_bins, rust_bins)?;
    let port = qemu::free_host_port();
    let (mut guest, mut console) = boot(bench, &staged, port, c_bins, rust_bins)?;

    let stalled = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
        .map_err(|e| format!("connect the reader that will not read: {e}"))?;
    let flood = guest.run_test(super::origin::FLOODER, Duration::from_secs(300));
    if flood.exit_code != Some(0) {
        return Err(format!("the flood exited {:?}", flood.exit_code));
    }
    let second = reader(port, "logstream-stalled-second.txt")?;
    if !second.wait_for(super::origin::FLOOD_DONE, CEILING) {
        return Err(format!(
            "a reader that connected after the flood, beside one that never read, did not \
             receive the flood's last line: {} line(s)",
            second.lines().len()
        ));
    }
    let file = shut_down(guest, &mut console, &staged)?;
    drop(stalled);
    second.wait_ended(CEILING);
    let received = second.lines();
    is_prefix_of(&received, &file)?;
    let floods = received.iter().filter(|l| l.contains("} flood ")).count();
    eprintln!(
        "  [stream] beside a reader that never read, a second one got {} line(s), {floods} of \
         them the flood's, each the line /log holds",
        received.len()
    );
    let _ = std::fs::remove_file(&staged.image);
    Ok(())
}
