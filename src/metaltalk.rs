//! The cable half of a metal boot: the listener a booting machine's `logd`
//! streams its records to, and the conversation the host then has with that
//! machine over ssh — a ping, one command whose answer is compared, and
//! `reboot`, which is how the host hands the machine back.
//!
//! **The machine names itself by connecting.** The stream's peer is the address
//! the boot leased, so nothing here is told it and nothing guesses it: the
//! ping and both ssh exchanges go to whoever opened the stream.
//!
//! **Every answer is recorded, including the ones that are not.** A boot that
//! never opened the stream, a command that was refused, a reboot the machine
//! went down under without a word — each is a [`Conversation`] field a judge
//! reads, and the loop that ran it hands the machine to its own fallback (the
//! boot's hold ends, the runner reboots) rather than stopping at the first.
//!
//! The ssh client is `tests/ssh-client-host`, russh from source: no host `ssh`
//! reaches ToyOS.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What the host asks the machine to say, and so what it must answer.
pub const PHRASE: &str = "the T14 answers over its own cable";

/// The command that hands the machine back: `/system/bin/reboot`, which the
/// boot config endows with `power`.
pub const REBOOT: &str = "reboot";

/// The port sshd listens on, which is the protocol's own.
pub const SSH_PORT: u16 = 22;

/// How many echo requests the machine gets to answer one.
const PING_TRIES: u32 = 10;
const PING_WAIT: Duration = Duration::from_secs(1);

/// How long the host keeps asking for the command once the stream is open.
///
/// **A liveness guard, not a measurement.** The stream opens the moment netd
/// has a lease and sshd may still be binding; what bounds the whole
/// conversation is the boot's own hold, and this is well inside it.
const EXEC_WINDOW: Duration = Duration::from_secs(30);
const EXEC_RETRY: Duration = Duration::from_secs(1);

/// The record stream, as this host receives it.
#[derive(Clone)]
pub struct Stream {
    shared: Arc<Shared>,
    /// Where it listens, the port a bind to `0` took included.
    at: SocketAddr,
}

struct Shared {
    lines: Mutex<Vec<String>>,
    peer: Mutex<Option<SocketAddr>>,
    /// The peer closed the connection, which is `logd` or netd going away.
    ended: AtomicBool,
    /// How the connection ended and how long after it opened, once it has.
    end: Mutex<Option<End>>,
    /// Set to stop waiting for a peer that has not come.
    stop: AtomicBool,
}

impl Stream {
    /// Bind `at`, before anything boots, and accept the one connection a boot
    /// opens. Every line is appended to `file` as it arrives — a machine that
    /// dies mid-boot leaves what it said on disk — and, with `echo`, printed.
    ///
    /// **A bind that fails is this host's answer and never the boot's**: an
    /// address this host does not hold is an image staged for another listener.
    pub fn listen(at: SocketAddr, file: &Path, echo: bool) -> Result<Self, String> {
        let socket = TcpListener::bind(at).map_err(|e| {
            format!("this host cannot listen at {at} ({e}): the image streams to an address it does not hold")
        })?;
        let at = socket
            .local_addr()
            .map_err(|e| format!("this host would not say where it listens at {at}: {e}"))?;
        socket
            .set_nonblocking(true)
            .map_err(|e| format!("this host would not poll its listener at {at}: {e}"))?;
        let mut out = std::fs::File::create(file)
            .map_err(|e| format!("{}: {e}", file.display()))?;
        let shared = Arc::new(Shared {
            lines: Mutex::new(Vec::new()),
            peer: Mutex::new(None),
            ended: AtomicBool::new(false),
            end: Mutex::new(None),
            stop: AtomicBool::new(false),
        });
        let theirs = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("metal-stream".into())
            .spawn(move || {
                let (conn, peer) = loop {
                    if theirs.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    match socket.accept() {
                        Ok(pair) => break pair,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        Err(_) => return,
                    }
                };
                // Accepted sockets inherit the listener's mode on this host's
                // BSD half; the reader below blocks.
                let _ = conn.set_nonblocking(false);
                *theirs.peer.lock().expect("the stream's peer") = Some(peer);
                if echo {
                    println!("  stream: {peer} connected");
                }
                let opened = Instant::now();
                let mut reader = BufReader::new(conn);
                let how = loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) => break "the peer closed it".to_string(),
                        Err(e) => break e.to_string(),
                        Ok(_) => {
                            let _ = out.write_all(line.as_bytes());
                            let _ = out.flush();
                            if echo {
                                print!("  stream| {line}");
                                let _ = std::io::stdout().flush();
                            }
                            theirs.lines.lock().expect("the stream's lines").push(line);
                        }
                    }
                };
                let end = End { after_ms: opened.elapsed().as_millis() as u64, how };
                if echo {
                    println!("  stream: {peer} ended {} ms after it opened: {}", end.after_ms, end.how);
                }
                *theirs.end.lock().expect("the stream's end") = Some(end);
                theirs.ended.store(true, Ordering::SeqCst);
            })
            .map_err(|e| format!("the stream's reader could not be started: {e}"))?;
        Ok(Self { shared, at })
    }

    pub fn local(&self) -> SocketAddr {
        self.at
    }

    pub fn peer(&self) -> Option<SocketAddr> {
        *self.shared.peer.lock().expect("the stream's peer")
    }

    pub fn lines(&self) -> Vec<String> {
        self.shared.lines.lock().expect("the stream's lines").clone()
    }

    pub fn ended(&self) -> bool {
        self.shared.ended.load(Ordering::SeqCst)
    }

    /// How the connection ended, or `None` while it is open or never opened.
    pub fn end(&self) -> Option<End> {
        self.shared.end.lock().expect("the stream's end").clone()
    }

    /// Stop waiting for a peer. A connection already accepted is read on.
    pub fn give_up(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
    }

    /// The peer, once one has connected, or `None` after `by` or once the
    /// host has given up on one.
    pub fn wait_connected(&self, by: Duration) -> Option<SocketAddr> {
        let began = Instant::now();
        loop {
            if let Some(peer) = self.peer() {
                return Some(peer);
            }
            if began.elapsed() >= by || self.shared.stop.load(Ordering::SeqCst) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

}

/// How a stream's connection ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct End {
    /// Milliseconds from the accept to the end, on this host's clock.
    pub after_ms: u64,
    /// The peer's close, or the error the read ended on.
    pub how: String,
}

/// What one exchange with the machine's sshd came back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exec {
    pub stdout: Vec<u8>,
    pub status: Option<u32>,
}

/// The harness's ssh client, and the key the image authorizes.
#[derive(Debug, Clone)]
pub struct Ssh {
    /// The repository whose `tests/ssh-client-host` built the client.
    root: PathBuf,
    key: PathBuf,
}

impl Ssh {
    /// The client this repository builds, refused by name where it is not
    /// built: `build::build_host_judges` is what makes it.
    pub fn at(root: &Path, key: PathBuf) -> Result<Self, String> {
        let client = crate::build::ssh_client_host(root);
        if !client.is_file() {
            return Err(format!(
                "{} is not built; the metal staging of a talking boot builds it",
                client.display()
            ));
        }
        if !key.is_file() {
            return Err(format!("{} is no key: the staging mints it beside the image", key.display()));
        }
        Ok(Self { root: root.to_path_buf(), key })
    }

    fn run(&self, argv: &[&str]) -> Result<String, String> {
        let out = Command::new(crate::build::ssh_client_host(&self.root))
            .args(argv)
            .output()
            .map_err(|e| format!("the ssh client would not start: {e}"))?;
        let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() {
            return Err(said);
        }
        Ok(said)
    }

    /// Run `command` and collect its stdout and status.
    pub fn exec(&self, at: SocketAddr, command: &str, scratch: &Path) -> Result<Exec, String> {
        let (out, err) = (scratch.join("exec.out"), scratch.join("exec.err"));
        let (host, port) = (at.ip().to_string(), at.port().to_string());
        let said = self.run(&[
            "exec",
            &host,
            &port,
            path_str(&self.key)?,
            path_str(&out)?,
            path_str(&err)?,
            command,
        ])?;
        let status = match said.lines().last().unwrap_or("") {
            "no-exit-status" => None,
            line => Some(
                line.strip_prefix("exit ")
                    .and_then(|code| code.parse().ok())
                    .ok_or_else(|| format!("the client answered {said:?}"))?,
            ),
        };
        let stdout = std::fs::read(&out).map_err(|e| format!("{}: {e}", out.display()))?;
        Ok(Exec { stdout, status })
    }

    /// Ask for `command` and answer the machine's reply to the request, without
    /// waiting for the program.
    pub fn fire(&self, at: SocketAddr, command: &str) -> Result<String, String> {
        let (host, port) = (at.ip().to_string(), at.port().to_string());
        let said = self.run(&["fire", &host, &port, path_str(&self.key)?, command])?;
        Ok(said.lines().last().unwrap_or("").to_string())
    }
}

fn path_str(path: &Path) -> Result<&str, String> {
    path.to_str().ok_or_else(|| format!("{} is not UTF-8", path.display()))
}

/// Everything the host heard from one boot over the cable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    /// Who opened the stream: the address the boot leased.
    pub peer: Ipv4Addr,
    /// Whether it answered a ping, or `None` where none was asked.
    pub ping: Option<bool>,
    /// What `echo` answered, or the client's last refusal after
    /// [`EXEC_WINDOW`] of asking.
    pub exec: Result<Exec, String>,
    /// The machine's reply to `reboot`, or the client's refusal.
    pub reboot: Result<String, String>,
    /// From the stream opening to the command's answer, on this host's clock.
    pub exec_ms: u64,
    /// How the stream's connection had ended when the conversation did, or
    /// `None` for one still open.
    pub stream_end: Option<End>,
}

/// The command asked, spelled once for the asker and the judge.
pub fn asked() -> String {
    format!("echo {PHRASE}")
}

/// The answer that command owes, byte for byte.
pub fn owed() -> Vec<u8> {
    format!("{PHRASE}\n").into_bytes()
}

/// Talk to the machine that opens `stream`: ping it, ask it to say
/// [`PHRASE`], and then ask it to reboot — whatever the first two said,
/// because handing the machine back is owed either way.
///
/// `ssh_at` is where sshd is reached, and `None` is the peer's own port 22;
/// QEMU's forward is the other case. `ping` is `false` where no ICMP can reach
/// the machine at all, which is QEMU's user-mode network.
///
/// `Err` is only a boot that never opened the stream within `by`.
pub fn converse(
    stream: &Stream,
    ssh: &Ssh,
    ssh_at: Option<SocketAddr>,
    ping: bool,
    by: Duration,
    scratch: &Path,
) -> Result<Conversation, String> {
    let peer = stream.wait_connected(by).ok_or_else(|| {
        format!("no boot opened the record stream within {} s", by.as_secs())
    })?;
    let SocketAddr::V4(peer_v4) = peer else {
        return Err(format!("the stream's peer is {peer}, which is no IPv4 address"));
    };
    let began = Instant::now();
    let peer = *peer_v4.ip();
    let ssh_at = ssh_at.unwrap_or(SocketAddr::V4(SocketAddrV4::new(peer, SSH_PORT)));

    let ping = if ping {
        let mut answered = false;
        for _ in 0..PING_TRIES {
            if crate::icmp::echo(peer, PING_WAIT)? {
                answered = true;
                break;
            }
        }
        println!("  talk: {peer} {} a ping", if answered { "answered" } else { "did not answer" });
        Some(answered)
    } else {
        None
    };

    std::fs::create_dir_all(scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    let command = asked();
    let mut exec = Err(String::from("never asked"));
    while began.elapsed() < EXEC_WINDOW {
        exec = ssh.exec(ssh_at, &command, scratch);
        match &exec {
            Ok(_) => break,
            Err(why) => {
                println!("  talk: {ssh_at} did not take `{command}` yet: {why}");
                std::thread::sleep(EXEC_RETRY);
            }
        }
    }
    let exec_ms = began.elapsed().as_millis() as u64;
    match &exec {
        Ok(got) => println!(
            "  talk: `{command}` answered {:?}, status {:?}, {exec_ms} ms after the stream opened",
            String::from_utf8_lossy(&got.stdout),
            got.status
        ),
        Err(why) => println!("  talk: `{command}` was never answered: {why}"),
    }

    let reboot = ssh.fire(ssh_at, REBOOT);
    println!("  talk: `{REBOOT}` {reboot:?}");
    Ok(Conversation { peer, ping, exec, reboot, exec_ms, stream_end: stream.end() })
}

/// The keys a conversation is written under, one `<key> <value>` per line, in
/// the loop's readback beside the host's other facts.
const PEER: &str = "talk_peer";
const PING: &str = "talk_ping";
const EXEC_STATUS: &str = "talk_exec_status";
const EXEC_STDOUT: &str = "talk_exec_stdout";
const EXEC_REFUSED: &str = "talk_exec_refused";
const EXEC_MS: &str = "talk_exec_ms";
const REBOOTED: &str = "talk_reboot";
const REBOOT_REFUSED: &str = "talk_reboot_refused";
const STREAM_END: &str = "talk_stream_end";

/// A value on one line, whatever it carried: Rust's own escaping, read back by
/// comparison with the same rendering rather than parsed.
fn one_line(text: &str) -> String {
    format!("{text:?}")
}

impl Conversation {
    pub fn render(&self) -> String {
        let mut out = format!("{PEER} {}\n", self.peer);
        match self.ping {
            Some(true) => out.push_str(&format!("{PING} yes\n")),
            Some(false) => out.push_str(&format!("{PING} no\n")),
            None => {}
        }
        match &self.exec {
            Ok(exec) => {
                match exec.status {
                    Some(code) => out.push_str(&format!("{EXEC_STATUS} {code}\n")),
                    None => out.push_str(&format!("{EXEC_STATUS} none\n")),
                }
                out.push_str(&format!(
                    "{EXEC_STDOUT} {}\n",
                    one_line(&String::from_utf8_lossy(&exec.stdout))
                ));
            }
            Err(why) => out.push_str(&format!("{EXEC_REFUSED} {}\n", one_line(why))),
        }
        out.push_str(&format!("{EXEC_MS} {}\n", self.exec_ms));
        match &self.stream_end {
            Some(end) => out.push_str(&format!("{STREAM_END} {} {}\n", end.after_ms, one_line(&end.how))),
            None => out.push_str(&format!("{STREAM_END} open\n")),
        }
        match &self.reboot {
            Ok(word) => out.push_str(&format!("{REBOOTED} {word}\n")),
            Err(why) => out.push_str(&format!("{REBOOT_REFUSED} {}\n", one_line(why))),
        }
        out
    }

    /// What a readback's boot file says about the cable's conversation, or
    /// `None` where the boot had none.
    pub fn parse(text: &str) -> Result<Option<Heard>, String> {
        let word = |key: &str| -> Option<String> {
            text.lines().find_map(|line| {
                let (name, rest) = line.split_once(' ')?;
                (name == key).then(|| rest.to_string())
            })
        };
        let Some(peer) = word(PEER) else { return Ok(None) };
        let peer = peer.parse().map_err(|_| format!("{PEER} reads {peer:?}"))?;
        let ping = match word(PING).as_deref() {
            Some("yes") => Some(true),
            Some("no") => Some(false),
            None => None,
            Some(other) => return Err(format!("{PING} reads {other:?}")),
        };
        let exec = match (word(EXEC_STATUS), word(EXEC_STDOUT), word(EXEC_REFUSED)) {
            (Some(status), Some(stdout), None) => Ok((status, stdout)),
            (None, None, Some(why)) => Err(why),
            _ => return Err(format!("the boot file's exec keys are not one answer:\n{text}")),
        };
        let reboot = match (word(REBOOTED), word(REBOOT_REFUSED)) {
            (Some(said), None) => Ok(said),
            (None, Some(why)) => Err(why),
            _ => return Err(format!("the boot file's reboot keys are not one answer:\n{text}")),
        };
        let exec_ms = word(EXEC_MS)
            .and_then(|ms| ms.parse().ok())
            .ok_or_else(|| format!("the boot file names no {EXEC_MS}"))?;
        let stream_end = match word(STREAM_END).as_deref() {
            Some("open") => None,
            Some(said) => {
                let (ms, how) = said.split_once(' ').unwrap_or((said, ""));
                let after_ms =
                    ms.parse().map_err(|_| format!("{STREAM_END} reads {said:?}"))?;
                Some(End { after_ms, how: how.to_string() })
            }
            None => return Err(format!("the boot file names no {STREAM_END}:\n{text}")),
        };
        Ok(Some(Heard { peer, ping, exec, reboot, exec_ms, stream_end }))
    }
}

/// A conversation as a readback carries it: the command's answer as rendered,
/// for comparison with [`Heard::answered_as_owed`] rather than for reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heard {
    pub peer: Ipv4Addr,
    pub ping: Option<bool>,
    /// `(status, stdout rendered)`, or the refusal rendered.
    pub exec: Result<(String, String), String>,
    pub reboot: Result<String, String>,
    pub exec_ms: u64,
    /// How the stream had ended when the conversation did, its reason as
    /// rendered; `None` for one still open.
    pub stream_end: Option<End>,
}

impl Heard {
    /// The command ran, ended 0, and said exactly [`PHRASE`] and a newline.
    pub fn answered_as_owed(&self) -> Result<(), String> {
        match &self.exec {
            Ok((status, stdout)) => {
                let want = one_line(&String::from_utf8_lossy(&owed()));
                if status != "0" || *stdout != want {
                    return Err(format!(
                        "`{}` answered {stdout} with status {status}, where {want} and 0 are owed",
                        asked()
                    ));
                }
                Ok(())
            }
            Err(why) => Err(format!("`{}` was never answered: {why}", asked())),
        }
    }

    /// The machine took `reboot`: it said so, or it went away under the
    /// request — which only the boot's own log can then tell from a machine
    /// that was already going.
    pub fn reboot_was_taken(&self) -> Result<&str, String> {
        match &self.reboot {
            Ok(word) if matches!(word.as_str(), "accepted" | "closed" | "silent") => Ok(word),
            Ok(word) => Err(format!("the machine answered `{REBOOT}` with {word:?}")),
            Err(why) => Err(format!("`{REBOOT}` could not be asked: {why}")),
        }
    }
}

/// What the host heard over the cable, judged: every finding, or what was
/// heard said in one line per fact.
///
/// **The stream is judged by what it carries, not by its length.** It owes the
/// boot's `Boot: complete` — the record that says the lines are this boot's —
/// and the peer that opened it is the machine that then answered the ping and
/// the command, because both were asked of that address and no other. What
/// the stream carries is kernel records alone; a daemon's own lines reach no
/// channel on a machine with no serial port, so netd's lease is not among them.
pub fn judge(heard: &Heard, stream: &[String]) -> Result<Vec<String>, Vec<String>> {
    let mut bad = Vec::new();
    let mut said = Vec::new();
    match crate::bootlog::boot_millis(&stream.concat()) {
        Some(ms) => said.push(format!(
            "{} record(s) arrived from {} over the cable, `Boot: complete` ({ms} ms) among them",
            stream.len(),
            heard.peer
        )),
        None => bad.push(format!(
            "{} record(s) arrived over the cable and none is this boot's `Boot: complete`",
            stream.len()
        )),
    }
    // **A connection this host accepted and then lost before a byte is this
    // host's own doing as often as the peer's**: macOS's application firewall
    // lets the handshake finish and then closes the socket of a binary it
    // blocks incoming connections for, which is what the T14's first talking
    // boot to open its stream met.
    if let (true, Some(end)) = (stream.is_empty(), &heard.stream_end) {
        bad.push(format!(
            "the connection from {} ended {} ms after this host accepted it, {}, before a byte \
             arrived: a host firewall that blocks this binary's incoming connections ends \
             one exactly so (on macOS: `/usr/libexec/ApplicationFirewall/socketfilterfw \
             --getappblocked <this binary>`)",
            heard.peer, end.after_ms, end.how
        ));
    }
    match heard.ping {
        Some(true) => said.push(format!("{} answered a ping", heard.peer)),
        Some(false) => bad.push(format!("{} answered no ping in {PING_TRIES} tries", heard.peer)),
        None => said.push("no ping was asked".to_string()),
    }
    match heard.answered_as_owed() {
        Ok(()) => said.push(format!(
            "`{}` answered byte for byte with status 0, {} ms after the stream opened",
            asked(),
            heard.exec_ms
        )),
        Err(why) => bad.push(why),
    }
    match heard.reboot_was_taken() {
        Ok(word) => said.push(format!("`{REBOOT}` was taken ({word})")),
        Err(why) => bad.push(why),
    }
    if bad.is_empty() {
        Ok(said)
    } else {
        Err(bad)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heard(exec: Result<Exec, String>, reboot: Result<String, String>) -> Heard {
        let said = Conversation {
            peer: Ipv4Addr::new(192, 168, 1, 49),
            ping: Some(true),
            exec,
            reboot,
            exec_ms: 812,
            stream_end: None,
        };
        Conversation::parse(&format!("back_secs 60\n{}stick_secs 0\n", said.render()))
            .expect("a rendered conversation reads back")
            .expect("and names a peer")
    }

    /// What the loop writes is what the judge reads, answer by answer, and the
    /// answer the command owes is compared byte for byte through the rendering.
    #[test]
    fn a_conversation_reads_back_as_it_was_written() {
        let good = heard(Ok(Exec { stdout: owed(), status: Some(0) }), Ok("accepted".into()));
        assert_eq!(good.peer, Ipv4Addr::new(192, 168, 1, 49));
        assert_eq!(good.ping, Some(true));
        assert_eq!(good.exec_ms, 812);
        good.answered_as_owed().expect("the owed answer");
        assert_eq!(good.reboot_was_taken(), Ok("accepted"));

        // One byte off, a status off, no status: each is a red of its own.
        let mut short = owed();
        short.pop();
        for (stdout, status) in [(short, Some(0)), (owed(), Some(1)), (owed(), None)] {
            let bad = heard(Ok(Exec { stdout, status }), Ok("accepted".into()));
            assert!(bad.answered_as_owed().is_err(), "{bad:?}");
        }
        let never = heard(Err("connecting: refused\nand more".into()), Ok("closed".into()));
        assert!(never.answered_as_owed().unwrap_err().contains("never answered"));
        assert_eq!(never.reboot_was_taken(), Ok("closed"));
        assert!(heard(Ok(Exec { stdout: owed(), status: Some(0) }), Ok("refused".into()))
            .reboot_was_taken()
            .is_err());
        assert!(heard(Ok(Exec { stdout: owed(), status: Some(0) }), Err("no key".into()))
            .reboot_was_taken()
            .is_err());
    }

    /// The judge wants the boot's own records on the wire and every answer the
    /// host asked for; each missing fact is a finding of its own.
    #[test]
    fn the_judge_reads_what_the_stream_carried_and_what_the_peer_answered() {
        let stream = vec![
            "[---------- -------- 0.011 cpu0] SMP: AP cpu1 online\n".to_string(),
            "[---------- -------- 1.216 cpu0] Boot: complete (1216ms)\n".to_string(),
        ];
        let good = heard(Ok(Exec { stdout: owed(), status: Some(0) }), Ok("accepted".into()));
        let said = judge(&good, &stream).expect("every fact is there");
        assert_eq!(said.len(), 4, "{said:?}");

        let bad = judge(&good, &stream[..1]).unwrap_err();
        assert!(bad.iter().any(|b| b.contains("Boot: complete")), "{bad:?}");
        let mut silent = good.clone();
        silent.ping = Some(false);
        assert_eq!(judge(&silent, &stream).unwrap_err().len(), 1);
        let mut unasked = good;
        unasked.ping = None;
        assert!(judge(&unasked, &stream).is_ok());
        let bad = judge(&heard(Err("refused".into()), Ok("refused".into())), &[]).unwrap_err();
        assert_eq!(bad.len(), 3, "{bad:?}");
    }

    /// **The T14's run 116 at the socket**: the peer is accepted, and the
    /// connection ends before a byte — which the stream records with how and
    /// when, and which the judge names as the host firewall's shape rather
    /// than as a boot that said nothing.
    #[test]
    fn a_connection_that_ends_before_a_byte_is_named_and_not_read_as_silence() {
        let dir = std::env::temp_dir().join(format!("metaltalk-cut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stream = Stream::listen("127.0.0.1:0".parse().unwrap(), &dir.join("s.log"), false)
            .expect("a loopback listener");
        drop(std::net::TcpStream::connect(stream.local()).unwrap());
        let began = Instant::now();
        while stream.end().is_none() && began.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let end = stream.end().expect("the end is recorded");
        assert_eq!(end.how, "the peer closed it");
        assert!(stream.lines().is_empty());

        let mut cut = heard(Ok(Exec { stdout: owed(), status: Some(0) }), Ok("accepted".into()));
        let said = Conversation {
            peer: cut.peer,
            ping: Some(true),
            exec: Ok(Exec { stdout: owed(), status: Some(0) }),
            reboot: Ok("accepted".into()),
            exec_ms: 457,
            stream_end: Some(end),
        };
        cut = Conversation::parse(&said.render()).unwrap().unwrap();
        let bad = judge(&cut, &[]).unwrap_err();
        assert!(bad.iter().any(|b| b.contains("host firewall")), "{bad:?}");
        // A stream that carried records and then ended is no such finding.
        let lines = vec!["[---------- -------- 1.216 cpu0] Boot: complete (1216ms)\n".to_string()];
        assert!(judge(&cut, &lines).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A boot file with no conversation is a boot that had none, and one with
    /// half of one is refused rather than read as either.
    #[test]
    fn a_boot_file_without_a_conversation_names_none_and_half_of_one_is_refused() {
        assert_eq!(Conversation::parse("back_secs 60\nstick_secs 0\n"), Ok(None));
        let half = "talk_peer 10.0.2.15\ntalk_exec_status 0\ntalk_exec_ms 5\ntalk_reboot accepted\n";
        assert!(Conversation::parse(half).is_err());
        let both = "talk_peer 10.0.2.15\ntalk_exec_refused \"x\"\ntalk_exec_ms 5\n\
                    talk_reboot accepted\ntalk_reboot_refused \"y\"\n";
        assert!(Conversation::parse(both).is_err());
    }

    /// The listener is the peer's naming: whoever connects is who the host then
    /// talks to, and every line arrives whole and in order.
    #[test]
    fn the_stream_names_its_peer_and_keeps_its_lines_in_order() {
        let dir = std::env::temp_dir().join(format!("metaltalk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("stream.log");
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = probe.local_addr().unwrap();
        drop(probe);
        let stream = Stream::listen(at, &file, false).expect("a loopback listener");
        assert_eq!(stream.wait_connected(Duration::from_millis(200)), None);
        let mut conn = std::net::TcpStream::connect(at).unwrap();
        for i in 0..100 {
            writeln!(conn, "[kernel 0.{i:03} cpu0] line {i}").unwrap();
        }
        drop(conn);
        let peer = stream.wait_connected(Duration::from_secs(5)).expect("the peer");
        assert_eq!(peer.ip(), at.ip());
        let began = Instant::now();
        while !stream.ended() && began.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let lines = stream.lines();
        assert_eq!(lines.len(), 100);
        for (i, line) in lines.iter().enumerate() {
            assert_eq!(*line, format!("[kernel 0.{i:03} cpu0] line {i}\n"));
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), lines.concat());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A peer that connects and says nothing yet is a stream, not an end.**
    /// A booting machine opens the connection before its next record exists;
    /// the reader waits for it rather than reading the silence as a close.
    #[test]
    fn a_peer_quiet_after_connecting_is_still_read() {
        let dir = std::env::temp_dir().join(format!("metaltalk-quiet-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stream = Stream::listen("127.0.0.1:0".parse().unwrap(), &dir.join("s.log"), false)
            .expect("a loopback listener");
        let mut conn = std::net::TcpStream::connect(stream.local()).unwrap();
        stream.wait_connected(Duration::from_secs(5)).expect("the peer");
        std::thread::sleep(Duration::from_millis(500));
        assert!(!stream.ended(), "a quiet peer was read as a closed one");
        writeln!(conn, "[kernel 1.216 cpu0] Boot: complete (1216ms)").unwrap();
        drop(conn);
        let began = Instant::now();
        while !stream.ended() && began.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(stream.lines(), vec!["[kernel 1.216 cpu0] Boot: complete (1216ms)\n"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An address this host does not hold is refused at the bind, before any
    /// machine is touched — the image was staged for some other listener.
    #[test]
    fn an_address_this_host_does_not_hold_is_refused_at_the_bind() {
        let file = std::env::temp_dir().join(format!("metaltalk-bind-{}.log", std::process::id()));
        // TEST-NET-1 (RFC 5737): assigned to no host.
        let at: SocketAddr = "192.0.2.1:41337".parse().unwrap();
        let refused = Stream::listen(at, &file, false).err().expect("no host holds TEST-NET-1");
        assert!(refused.contains("does not hold"), "{refused}");
        let _ = std::fs::remove_file(&file);
    }
}
