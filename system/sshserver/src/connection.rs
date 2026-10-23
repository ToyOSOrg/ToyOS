//! One client's connection: a thread that owns the protocol, and the threads
//! that serve it.
//!
//! **Everything reaches the connection's thread as an [`Input`] on one queue**:
//! the client's bytes from the socket's reader, a program's output from its
//! pipes' readers, what its stdin writer took, and its exit. That thread alone
//! touches the [`Server`] and writes the socket, so no lock orders the two.
//!
//! **Flow control is the protocol's, end to end.** The client's window comes
//! back only as a program's stdin takes the bytes, so a program that reads
//! nothing stops its own channel and no other. A pipe's reader waits for its
//! last chunk to be taken into the client's window before it reads again, so
//! a program that writes faster than the client reads waits on its pipe. The
//! socket's reader waits the same way for its last chunk to be taken, so a
//! client that sends faster than the connection answers waits on its socket.
//!
//! **Until the client logs in, the deadline bounds everything**: the wait for
//! its bytes and every write to it, so a client that stops reading is closed
//! at the deadline like one that stops sending.
//!
//! **Nothing this connection starts outlives it**: at its end every program it
//! started is ended, and every thread it started ends with its queue.

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use toyos_ssh::{Authorizer, ChannelId, Event, HostKey, Server};

use crate::program::Program;
use crate::spawn::Login;
use crate::{sftp, OsRandom};

/// This daemon would not run what was asked: the shell's own convention for
/// a command that could not be executed, and the reason is on stderr.
const EXIT_REFUSED: u32 = 127;

/// The program ran and this daemon cannot say how it ended.
pub const EXIT_LOST: u32 = 254;

/// The most a pipe's reader takes at once.
const CHUNK: usize = 32 * 1024;

pub enum Input {
    /// The client's next bytes, and the reader to tell once they are taken.
    Socket { bytes: Vec<u8>, taken: SyncSender<()> },
    /// The client's side is closed.
    SocketEnd,
    /// A program's output, and the reader to tell once it is in the client's
    /// window.
    Output { channel: ChannelId, stderr: bool, data: Vec<u8>, taken: SyncSender<()> },
    /// One of a program's output streams ended.
    OutputEnd { channel: ChannelId },
    /// A program took `n` bytes of its input.
    Consumed { channel: ChannelId, n: usize },
    /// A program ended with this status.
    Exited { channel: ChannelId, status: u32 },
}

/// A chunk of a program's output waiting for the client's window.
struct Pending {
    stderr: bool,
    data: Vec<u8>,
    sent: usize,
    taken: SyncSender<()>,
}

/// A channel's output waiting for the client's window, oldest first.
#[derive(Default)]
struct Output(VecDeque<Pending>);

impl Output {
    fn push(&mut self, stderr: bool, data: Vec<u8>, taken: SyncSender<()>) {
        self.0.push_back(Pending { stderr, data, sent: 0, taken });
    }

    /// Hand `send` what waits, oldest first, as much as it takes, and tell
    /// each reader whose chunk went whole. `send` answers how much it took.
    fn flush<E>(&mut self, mut send: impl FnMut(bool, &[u8]) -> Result<usize, E>) {
        while let Some(front) = self.0.front_mut() {
            let rest = &front.data[front.sent..];
            match send(front.stderr, rest) {
                Ok(n) if n == rest.len() => {
                    let done = self.0.pop_front().expect("the front");
                    let _ = done.taken.send(());
                }
                Ok(n) => {
                    front.sent += n;
                    return;
                }
                Err(_) => return,
            }
        }
    }
}

/// What runs on a channel.
struct Running {
    /// Where the client's bytes go; `None` after its EOF, or once the
    /// program stopped reading, when they are consumed unread.
    input: Option<Sender<Vec<u8>>>,
    output: Output,
    /// Output streams not yet at their end.
    streams: u8,
    status: Option<u32>,
    /// `\n` becomes `\r\n` on the way out: a shell asked for with a terminal.
    crlf: bool,
    /// The program, and its name for the console; `None` for `sftp`, which
    /// runs in this daemon.
    program: Option<(Arc<Program>, String)>,
}

/// Serve one client: `authorizer` names who may log in, and the client has
/// until `deadline` to do it, holding `unauthenticated` until then.
pub fn serve<A: Authorizer>(stream: TcpStream, host: Arc<HostKey>, authorizer: A, peer: String, unauthenticated: crate::Unauthenticated, deadline: Duration) {
    let (tx, rx) = mpsc::channel();
    let reader = stream.try_clone().expect("a socket's second handle");
    {
        let tx = tx.clone();
        std::thread::spawn(move || read_socket(reader, tx));
    }
    let mut connection = Connection {
        server: Server::new(host, authorizer, OsRandom),
        socket: stream,
        tx,
        channels: BTreeMap::new(),
        peer,
        login: None,
    };
    connection.run(&rx, unauthenticated, deadline);
    connection.end();
}

/// The client's bytes, one chunk in flight at a time.
fn read_socket(mut socket: TcpStream, tx: Sender<Input>) {
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = socket.read(&mut buf).unwrap_or(0);
        if n == 0 {
            let _ = tx.send(Input::SocketEnd);
            return;
        }
        let (taken, wait) = mpsc::sync_channel(1);
        if tx.send(Input::Socket { bytes: buf[..n].to_vec(), taken }).is_err() || wait.recv().is_err() {
            return;
        }
    }
}

struct Connection<A> {
    server: Server<A, OsRandom>,
    socket: TcpStream,
    tx: Sender<Input>,
    channels: BTreeMap<ChannelId, Running>,
    peer: String,
    /// The login session this connection's programs run in, opened with the
    /// first of them and signed out when the connection ends.
    login: Option<Login>,
}

impl<A: Authorizer> Connection<A> {
    fn run(&mut self, rx: &Receiver<Input>, unauthenticated: crate::Unauthenticated, login: Duration) {
        let deadline = Instant::now() + login;
        let mut unauthenticated = Some(unauthenticated);
        let late = |peer: &str| println!("sshserver: {peer}: no login within {}s; closing", login.as_secs());
        // The identification line and KEXINIT, before the client says a word.
        if !self.write_out(Some(deadline)) {
            return late(&self.peer);
        }
        loop {
            let input = match unauthenticated {
                Some(_) => match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(input) => input,
                    Err(_) => return late(&self.peer),
                },
                None => rx.recv().expect("the connection holds a sender of its own queue"),
            };
            if !self.input(input) {
                // A refusal's DISCONNECT, if there is one.
                self.write_out(unauthenticated.is_some().then_some(deadline));
                return;
            }
            while let Some(event) = self.server.poll() {
                if matches!(event, Event::Authenticated { .. }) {
                    unauthenticated = None;
                }
                if !self.event(event) {
                    return;
                }
            }
            if !self.write_out(unauthenticated.is_some().then_some(deadline)) {
                if unauthenticated.is_some() && Instant::now() >= deadline {
                    late(&self.peer);
                }
                return;
            }
        }
    }

    /// Write what the protocol has for the client, all of it by `deadline` if
    /// there is one: whether all of it went. A write timeout bounds one send,
    /// so each send is given only the time left.
    fn write_out(&mut self, deadline: Option<Instant>) -> bool {
        let out = self.server.output();
        write_by(&mut self.socket, TcpStream::set_write_timeout, &out, deadline)
    }

    /// Whether the connection goes on.
    fn input(&mut self, input: Input) -> bool {
        match input {
            Input::SocketEnd => return false,
            Input::Socket { bytes, taken } => {
                let result = self.server.input(&bytes);
                let _ = taken.send(());
                if let Err(refusal) = result {
                    println!("sshserver: {}: refused: {refusal}", self.peer);
                    return false;
                }
            }
            Input::Output { channel, stderr, data, taken } => {
                let Some(running) = self.channels.get_mut(&channel) else { return true };
                let data = if running.crlf { crlf(&data) } else { data };
                running.output.push(stderr, data, taken);
                self.flush(channel);
            }
            Input::OutputEnd { channel } => {
                let Some(running) = self.channels.get_mut(&channel) else { return true };
                running.streams -= 1;
                // Both output streams are at end of file: wait for the
                // status. Until now `end` could kill the program; a program
                // that closed both and runs on is waited for.
                if let (0, Some((program, name))) = (running.streams, &running.program) {
                    let (program, name, tx) = (program.clone(), name.clone(), self.tx.clone());
                    std::thread::spawn(move || {
                        let _ = tx.send(Input::Exited { channel, status: program.wait(&name) });
                    });
                }
                self.finish(channel);
            }
            Input::Consumed { channel, n } => {
                if self.channels.contains_key(&channel) {
                    let _ = self.server.consumed(channel, n);
                }
            }
            Input::Exited { channel, status } => {
                if let Some(running) = self.channels.get_mut(&channel) {
                    running.status = Some(status);
                    self.finish(channel);
                }
            }
        }
        true
    }

    /// Whether the connection goes on.
    fn event(&mut self, event: Event) -> bool {
        match event {
            Event::Authenticated { user, fingerprint } => println!("sshserver: {user} authenticated with {fingerprint}"),
            Event::Exec { channel, command } => self.exec(channel, &command),
            Event::Shell { channel, pty } => self.start(channel, vec![SHELL.to_string()], pty),
            Event::Sftp { channel } => self.sftp(channel),
            Event::Data { channel, data } => {
                let n = data.len();
                let taken = self.channels.get_mut(&channel).and_then(|r| r.input.as_ref()).is_some_and(|input| input.send(data).is_ok());
                if !taken {
                    if let Some(running) = self.channels.get_mut(&channel) {
                        running.input = None;
                    }
                    let _ = self.server.consumed(channel, n);
                }
            }
            Event::Eof { channel } => {
                if let Some(running) = self.channels.get_mut(&channel) {
                    running.input = None;
                }
            }
            Event::Closed { channel } => {
                if let Some(running) = self.channels.remove(&channel) {
                    end(running);
                }
            }
            Event::Writable { channel } => self.flush(channel),
            Event::Declined(declined) => println!("sshserver: {}: declined {declined:?}", self.peer),
            Event::Disconnected => return false,
        }
        true
    }
}

const SHELL: &str = "/system/bin/shell";

impl<A: Authorizer> Connection<A> {
    /// Run the request line as `shell -c` does, as OpenSSH runs it under the
    /// user's `$SHELL -c`: SSH has no argument vector, and every client pastes
    /// its words into one line for a shell to read. Its quoting is the shell's.
    fn exec(&mut self, channel: ChannelId, line: &[u8]) {
        match std::str::from_utf8(line) {
            Ok(line) => self.start(channel, vec![SHELL.to_string(), "-c".to_string(), line.to_string()], false),
            Err(_) => self.refuse(channel, "the command is not UTF-8"),
        }
    }

    /// End `channel` with `why` on its stderr and [`EXIT_REFUSED`].
    fn refuse(&mut self, channel: ChannelId, why: &str) {
        println!("sshserver: {}: refused: {why}", self.peer);
        let _ = self.server.send_stderr(channel, format!("sshserver: {why}\r\n").as_bytes());
        let _ = self.server.exit(channel, EXIT_REFUSED);
    }

    fn start(&mut self, channel: ChannelId, argv: Vec<String>, crlf: bool) {
        let login = match self.login.take().map_or_else(Login::open, Ok) {
            Ok(login) => self.login.insert(login),
            Err(why) => return self.refuse(channel, &format!("no session to run {} in: {why}", argv[0])),
        };
        let spawned = login
            .command(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => return self.refuse(channel, &format!("cannot run {}: {e}", argv[0])),
        };
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let program = match Program::new(child) {
            Ok(program) => Arc::new(program),
            Err((mut child, why)) => {
                let _ = child.kill();
                let _ = child.wait();
                return self.refuse(channel, &format!("{} started and {why}", argv[0]));
            }
        };
        let (input, queue) = mpsc::channel();
        let tx = self.tx.clone();
        std::thread::spawn(move || write_stdin(stdin, queue, channel, tx));
        pump(stdout, false, channel, self.tx.clone());
        pump(stderr, true, channel, self.tx.clone());
        let name = argv.into_iter().next().expect("a program");
        let running = Running { input: Some(input), output: Output::default(), streams: 2, status: None, crlf, program: Some((program, name)) };
        self.channels.insert(channel, running);
    }

    /// Serve the `sftp` subsystem on its own thread: whole requests in, whole
    /// replies out.
    fn sftp(&mut self, channel: ChannelId) {
        let (input, queue) = mpsc::channel::<Vec<u8>>();
        let (tx, peer) = (self.tx.clone(), self.peer.clone());
        std::thread::spawn(move || {
            let mut server = sftp::Server::new();
            let mut buf = Vec::new();
            let status = 'serve: loop {
                let Ok(chunk) = queue.recv() else { break 0 };
                let n = chunk.len();
                buf.extend_from_slice(&chunk);
                loop {
                    let packet = match sftp::next_packet(&mut buf) {
                        Ok(Some(packet)) => packet,
                        Ok(None) => break,
                        Err(fatal) => break 'serve fatal.say(&peer),
                    };
                    let reply = match server.request(&packet) {
                        Ok(reply) => reply,
                        Err(fatal) => break 'serve fatal.say(&peer),
                    };
                    if !send(&tx, channel, false, reply) {
                        return;
                    }
                }
                if tx.send(Input::Consumed { channel, n }).is_err() {
                    return;
                }
            };
            let _ = tx.send(Input::OutputEnd { channel });
            let _ = tx.send(Input::Exited { channel, status });
        });
        self.channels.insert(channel, Running { input: Some(input), output: Output::default(), streams: 1, status: None, crlf: false, program: None });
    }

    /// Hand `channel`'s waiting output to the client, as much as its window
    /// takes, and tell each reader whose chunk went.
    fn flush(&mut self, channel: ChannelId) {
        let Self { channels, server, .. } = self;
        let Some(running) = channels.get_mut(&channel) else { return };
        running.output.flush(|stderr, data| match stderr {
            false => server.send(channel, data),
            true => server.send_stderr(channel, data),
        });
    }

    /// Say how the program ended once both its streams have: a reader ends
    /// only after its last chunk was taken, so all its output is out.
    fn finish(&mut self, channel: ChannelId) {
        let Some(running) = self.channels.get(&channel) else { return };
        if let (0, Some(status)) = (running.streams, running.status) {
            assert!(running.output.0.is_empty(), "a stream ended with its output still waiting");
            self.channels.remove(&channel);
            let _ = self.server.exit(channel, status);
        }
    }

    fn end(self) {
        let _ = self.socket.shutdown(Shutdown::Both);
        for (_, running) in self.channels {
            end(running);
        }
    }
}

/// Write all of `bytes` to `sink`, by `deadline` if there is one: whether all
/// of it went. `set_timeout` bounds the next send, and is given the time left
/// before each one.
fn write_by<W: Write>(sink: &mut W, set_timeout: impl Fn(&W, Option<Duration>) -> std::io::Result<()>, bytes: &[u8], deadline: Option<Instant>) -> bool {
    let mut rest = bytes;
    while !rest.is_empty() {
        let timeout = match deadline.map(|at| at.saturating_duration_since(Instant::now())) {
            Some(Duration::ZERO) => return false,
            timeout => timeout,
        };
        if set_timeout(sink, timeout).is_err() {
            return false;
        }
        match sink.write(rest) {
            Ok(0) => return false,
            Ok(n) => rest = &rest[n..],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    true
}

/// Let go of what runs on a channel: its program is asked to quit
/// ([`Program::end`]), and its pipes then end its readers once it does.
fn end(running: Running) {
    if let Some((program, _)) = running.program {
        program.end();
    }
}

/// One output stream of a program onto the connection's queue, one chunk in
/// flight at a time.
fn pump(mut pipe: impl Read + Send + 'static, stderr: bool, channel: ChannelId, tx: Sender<Input>) {
    std::thread::spawn(move || {
        let mut buf = vec![0; CHUNK];
        while let Ok(n @ 1..) = pipe.read(&mut buf) {
            if !send(&tx, channel, stderr, buf[..n].to_vec()) {
                return;
            }
        }
        let _ = tx.send(Input::OutputEnd { channel });
    });
}

/// Queue output and wait until it is in the client's window: `false` once
/// the connection or the channel is gone.
fn send(tx: &Sender<Input>, channel: ChannelId, stderr: bool, data: Vec<u8>) -> bool {
    let (taken, wait) = mpsc::sync_channel(1);
    tx.send(Input::Output { channel, stderr, data, taken }).is_ok() && wait.recv().is_ok()
}

/// The client's bytes into a program's stdin, each chunk consumed once
/// written; once the program stops reading, consumed unwritten, so the
/// client's window is not lost with it. The queue's end is the program's EOF.
fn write_stdin(stdin: impl Write, queue: Receiver<Vec<u8>>, channel: ChannelId, tx: Sender<Input>) {
    let mut stdin = Some(stdin);
    for chunk in queue {
        if stdin.as_mut().is_some_and(|pipe| pipe.write_all(&chunk).is_err()) {
            stdin = None;
        }
        if tx.send(Input::Consumed { channel, n: chunk.len() }).is_err() {
            return;
        }
    }
}

/// `\n` to `\r\n`, for the one channel that has a terminal on the far end:
/// this system has no terminal layer to do it.
fn crlf(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 2);
    for &b in data {
        if b == b'\n' {
            out.push(b'\r');
        }
        out.push(b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_gets_a_carriage_return_before_every_line_feed() {
        assert_eq!(crlf(b"a\nb\n\nc"), b"a\r\nb\r\n\r\nc");
        assert_eq!(crlf(b""), b"");
    }

    /// A chunk the window takes in part waits for the rest, in order, and its
    /// reader is told only once all of it went.
    #[test]
    fn output_goes_whole_and_in_order_through_a_narrow_window() {
        let mut output = Output::default();
        let (first, first_taken) = mpsc::sync_channel(1);
        let (second, second_taken) = mpsc::sync_channel(1);
        output.push(false, b"abcdefghijklmnopqrstuvwxyz".to_vec(), first);
        output.push(true, b"123".to_vec(), second);
        let mut sent: Vec<(bool, Vec<u8>)> = Vec::new();
        // A window of 10 bytes for each flush.
        let mut flush = |output: &mut Output| {
            let mut room = 10;
            output.flush(|stderr, data: &[u8]| -> Result<usize, ()> {
                let n = data.len().min(room);
                room -= n;
                sent.push((stderr, data[..n].to_vec()));
                Ok(n)
            });
        };
        flush(&mut output);
        assert!(first_taken.try_recv().is_err(), "a chunk in part is not taken");
        flush(&mut output);
        flush(&mut output);
        assert!(first_taken.try_recv().is_ok() && second_taken.try_recv().is_ok(), "both chunks went whole");
        let stdout: Vec<u8> = sent.iter().filter(|(e, _)| !e).flat_map(|(_, d)| d.clone()).collect();
        let stderr: Vec<u8> = sent.iter().filter(|(e, _)| *e).flat_map(|(_, d)| d.clone()).collect();
        assert_eq!((stdout.as_slice(), stderr.as_slice()), (&b"abcdefghijklmnopqrstuvwxyz"[..], &b"123"[..]));
        assert!(output.0.is_empty());
    }

    /// A reader too slow to keep up: one byte a send, each send 20 ms.
    struct Trickle(Vec<u8>);

    impl Write for Trickle {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            // The pace is the stimulus: every send makes progress.
            std::thread::sleep(Duration::from_millis(20));
            self.0.push(bytes[0]);
            Ok(1)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// **The deadline bounds the whole write, not one send**: a reader that
    /// takes a byte at a time is given up at the deadline, though every send
    /// made progress within its timeout.
    #[test]
    fn a_write_to_a_slow_reader_ends_at_the_deadline() {
        let mut sink = Trickle(Vec::new());
        let start = Instant::now();
        let deadline = start + Duration::from_millis(200);
        let wrote = write_by(&mut sink, |_, _| Ok(()), &[7; 200], Some(deadline));
        assert!(!wrote, "the write outlived its deadline: {} bytes in {:?}", sink.0.len(), start.elapsed());
        assert!(sink.0.len() < 20, "{} bytes went after the deadline", sink.0.len());
        // With no deadline, all of it goes.
        let mut sink = Trickle(Vec::new());
        assert!(write_by(&mut sink, |_, _| Ok(()), &[7; 5], None));
        assert_eq!(sink.0, [7; 5]);
    }

    /// Names every key, so every offer is answered `PK_OK` and costs nothing.
    struct Everyone;

    impl Authorizer for Everyone {
        fn authorizes(&mut self, _user: &str, _key: &[u8; 32]) -> bool {
            true
        }
    }

    fn string(out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(bytes);
    }

    fn plain_packet(payload: &[u8]) -> Vec<u8> {
        let mut padding = 8 - (5 + payload.len()) % 8;
        if padding < 4 {
            padding += 8;
        }
        let mut out = ((1 + payload.len() + padding) as u32).to_be_bytes().to_vec();
        out.push(padding as u8);
        out.extend_from_slice(payload);
        out.extend(std::iter::repeat_n(0, padding));
        out
    }

    fn sealed_packet(key: &mut toyos_crypto::ssh::SealingKey, payload: &[u8]) -> Vec<u8> {
        let mut padding = 8 - (1 + payload.len()) % 8;
        if padding < 4 {
            padding += 8;
        }
        let mut out = ((1 + payload.len() + padding) as u32).to_be_bytes().to_vec();
        out.push(padding as u8);
        out.extend_from_slice(payload);
        out.extend(std::iter::repeat_n(0, padding));
        let (header, body) = out.split_first_chunk_mut::<4>().unwrap();
        let (_, tag) = key.seal(header, body).unwrap();
        out.extend_from_slice(&tag);
        out
    }

    /// The next plaintext packet's payload from `socket`, `buf` holding what
    /// was read past it.
    fn next_plain(socket: &mut TcpStream, buf: &mut Vec<u8>) -> Vec<u8> {
        loop {
            if let Some(len) = buf.first_chunk::<4>().map(|l| u32::from_be_bytes(*l) as usize)
                && buf.len() >= 4 + len
            {
                let packet: Vec<u8> = buf.drain(..4 + len).collect();
                let padding = packet[4] as usize;
                return packet[5..packet.len() - padding].to_vec();
            }
            let mut chunk = [0u8; 4096];
            let n = socket.read(&mut chunk).expect("the server's bytes");
            assert!(n > 0, "the server closed during the exchange");
            buf.extend_from_slice(&chunk[..n]);
        }
    }

    fn sha256(parts: &[&[u8]]) -> [u8; 32] {
        let mut c = toyos_crypto::hash::Sha256::new();
        for p in parts {
            c.update(p).unwrap();
        }
        c.finalize()
    }

    /// A client that logs in to nothing: it completes the exchange, then sends
    /// unsigned offers, each answered `PK_OK`, and never reads, until its
    /// writes fail.
    fn flood(mut socket: TcpStream) {
        let mut buf = Vec::new();
        let line = loop {
            if let Some(end) = buf.windows(2).position(|w| w == b"\r\n") {
                break buf.drain(..end + 2).take(end).collect::<Vec<u8>>();
            }
            let mut chunk = [0u8; 4096];
            let n = socket.read(&mut chunk).expect("the server's identification");
            assert!(n > 0, "the server closed before its identification");
            buf.extend_from_slice(&chunk[..n]);
        };
        let i_s = next_plain(&mut socket, &mut buf);
        let v_c = b"SSH-2.0-Flood";
        let mut i_c = vec![20];
        i_c.extend_from_slice(&[0x11; 16]);
        for list in ["curve25519-sha256,kex-strict-c-v00@openssh.com", "ssh-ed25519", "chacha20-poly1305@openssh.com", "chacha20-poly1305@openssh.com", "", "", "none", "none", "", ""] {
            string(&mut i_c, list.as_bytes());
        }
        i_c.extend_from_slice(&[0; 5]);
        let private = toyos_crypto::x25519::SecretKey::from_bytes([0x3c; 32]);
        let q_c = private.public_key().0;
        let mut init = vec![30];
        string(&mut init, &q_c);
        let hello = [&v_c[..], b"\r\n", &plain_packet(&i_c), &plain_packet(&init)].concat();
        socket.write_all(&hello).expect("the client's half of the exchange");
        let reply = next_plain(&mut socket, &mut buf);
        assert_eq!(next_plain(&mut socket, &mut buf), [21], "NEWKEYS");
        let mut at = &reply[1..];
        let mut field = || {
            let len = u32::from_be_bytes(at[..4].try_into().unwrap()) as usize;
            let s = at[4..4 + len].to_vec();
            at = &at[4 + len..];
            s
        };
        let (k_s, q_s) = (field(), field());
        let shared = private.agree(&toyos_crypto::x25519::PublicKey(q_s[..].try_into().unwrap())).unwrap();
        let digits: Vec<u8> = shared.as_bytes().iter().copied().skip_while(|&b| b == 0).collect();
        let mut k = Vec::new();
        string(&mut k, &[&[0u8][..], &digits].concat()[usize::from(digits[0] & 0x80 == 0)..]);
        let mut hashed = Vec::new();
        for f in [&v_c[..], &line, &i_c, &i_s, &k_s, &q_c, &q_s] {
            string(&mut hashed, f);
        }
        hashed.extend_from_slice(&k);
        let h = sha256(&[&hashed]);
        let k1 = sha256(&[&k, &h, b"C", &h]);
        let k2 = sha256(&[&k, &h, &k1]);
        let mut sealing = toyos_crypto::ssh::SealingKey::new([k1, k2].concat().try_into().unwrap(), 0);
        socket.write_all(&plain_packet(&[21])).expect("NEWKEYS");
        let mut service = vec![5];
        string(&mut service, b"ssh-userauth");
        let mut offer = vec![50];
        string(&mut offer, b"toyos");
        string(&mut offer, b"ssh-connection");
        string(&mut offer, b"publickey");
        offer.push(0);
        string(&mut offer, b"ssh-ed25519");
        let mut blob = Vec::new();
        string(&mut blob, b"ssh-ed25519");
        string(&mut blob, &[9; 32]);
        string(&mut offer, &blob);
        let mut packet = sealed_packet(&mut sealing, &service);
        while socket.write_all(&packet).is_ok() {
            packet = sealed_packet(&mut sealing, &offer);
        }
    }

    /// **A client that stops reading is closed at the deadline.** It floods
    /// offers that cost it nothing and never reads their answers, so the
    /// server's writes fill both sides' buffers; the connection still ends at
    /// its deadline, and gives its place back.
    #[test]
    fn a_client_that_floods_offers_and_never_reads_is_closed_at_the_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let place = crate::Unauthenticated::take(&count).expect("a place");
        let login = Duration::from_secs(2);
        let (done, served) = mpsc::channel();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("the flood's connect");
            let host = Arc::new(HostKey::from_seed(&[7; 32]));
            let start = Instant::now();
            serve(stream, host, Everyone, "the flood".into(), place, login);
            let _ = done.send(start.elapsed());
        });
        let client = std::thread::spawn(move || flood(TcpStream::connect(addr).expect("connect")));
        let took = served.recv_timeout(Duration::from_secs(60)).expect("the connection ended within a minute of a 2 s deadline");
        assert!(took >= login, "closed after {took:?}, before the deadline: the flood was refused, not held");
        assert_eq!(count.load(std::sync::atomic::Ordering::Acquire), 0, "the place is given back");
        client.join().expect("the flood ends once its connection is closed");
    }
}
