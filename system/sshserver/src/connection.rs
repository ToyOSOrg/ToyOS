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
//! a program that writes faster than the client reads waits on its pipe.
//!
//! **Nothing this connection starts outlives it**: at its end every program it
//! started is ended, and every thread it started ends with its queue.

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use toyos_ssh::{ChannelId, Event, HostKey, Server};

use crate::program::Program;
use crate::{command, sftp, Keys, OsRandom};

/// How long a client has to authenticate: OpenSSH's `LoginGraceTime`. It is
/// what bounds a client that only offers keys, which costs it no attempt.
pub const LOGIN_DEADLINE: Duration = Duration::from_secs(120);

/// This daemon would not run what was asked: the shell's own convention for
/// a command that could not be executed, and the reason is on stderr.
const EXIT_REFUSED: u32 = 127;

/// The program ran and this daemon cannot say how it ended.
pub const EXIT_LOST: u32 = 254;

/// The most a pipe's reader takes at once.
const CHUNK: usize = 32 * 1024;

pub enum Input {
    /// The client's next bytes, or `None` once its side is closed.
    Socket(Option<Vec<u8>>),
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

/// A program's output waiting for the client's window.
struct Pending {
    stderr: bool,
    data: Vec<u8>,
    sent: usize,
    taken: SyncSender<()>,
}

/// What runs on a channel.
struct Running {
    /// Where the client's bytes go; `None` after its EOF, or once the
    /// program stopped reading, when they are consumed unread.
    input: Option<Sender<Vec<u8>>>,
    output: VecDeque<Pending>,
    /// Output streams not yet at their end.
    streams: u8,
    status: Option<u32>,
    /// `\n` becomes `\r\n` on the way out: a shell asked for with a terminal.
    crlf: bool,
    /// The program, and its name for the console; `None` for `sftp`, which
    /// runs in this daemon.
    program: Option<(Arc<Program>, String)>,
}

pub fn serve(stream: TcpStream, host: Arc<HostKey>, peer: String, unauthenticated: crate::Unauthenticated) {
    let (tx, rx) = mpsc::channel();
    let reader = stream.try_clone().expect("a socket's second handle");
    {
        let tx = tx.clone();
        std::thread::spawn(move || read_socket(reader, tx));
    }
    let mut connection = Connection {
        server: Server::new(host, Keys, OsRandom),
        socket: stream,
        tx,
        channels: BTreeMap::new(),
        peer,
    };
    connection.run(&rx, unauthenticated);
    connection.end();
}

fn read_socket(mut socket: TcpStream, tx: Sender<Input>) {
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = socket.read(&mut buf).unwrap_or(0);
        let bytes = (n > 0).then(|| buf[..n].to_vec());
        if tx.send(Input::Socket(bytes)).is_err() || n == 0 {
            return;
        }
    }
}

struct Connection {
    server: Server<Keys, OsRandom>,
    socket: TcpStream,
    tx: Sender<Input>,
    channels: BTreeMap<ChannelId, Running>,
    peer: String,
}

impl Connection {
    fn run(&mut self, rx: &Receiver<Input>, unauthenticated: crate::Unauthenticated) {
        let deadline = Instant::now() + LOGIN_DEADLINE;
        let mut unauthenticated = Some(unauthenticated);
        // The identification line and KEXINIT, before the client says a word.
        if self.socket.write_all(&self.server.output()).is_err() {
            return;
        }
        loop {
            let input = match unauthenticated {
                Some(_) => match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(input) => input,
                    Err(_) => {
                        println!("sshserver: {}: no login within {}s; closing", self.peer, LOGIN_DEADLINE.as_secs());
                        return;
                    }
                },
                None => rx.recv().expect("the connection holds a sender of its own queue"),
            };
            if !self.input(input) {
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
            let out = self.server.output();
            if !out.is_empty() && self.socket.write_all(&out).is_err() {
                return;
            }
        }
    }

    /// Whether the connection goes on.
    fn input(&mut self, input: Input) -> bool {
        match input {
            Input::Socket(None) => return false,
            Input::Socket(Some(bytes)) => {
                if let Err(refusal) = self.server.input(&bytes) {
                    println!("sshserver: {}: refused: {refusal}", self.peer);
                    let _ = self.socket.write_all(&self.server.output());
                    return false;
                }
            }
            Input::Output { channel, stderr, data, taken } => {
                let Some(running) = self.channels.get_mut(&channel) else { return true };
                let data = if running.crlf { crlf(&data) } else { data };
                running.output.push_back(Pending { stderr, data, sent: 0, taken });
                self.flush(channel);
            }
            Input::OutputEnd { channel } => {
                let Some(running) = self.channels.get_mut(&channel) else { return true };
                running.streams -= 1;
                // Both pipes are at their end, which the program's own end is
                // what does: wait for its status now, while it can still be
                // ended (`program`).
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

impl Connection {
    /// Run the named program. **Not through a shell**: `command::split` is the
    /// whole grammar the line is read with, and a client that wants a pipe asks
    /// for `shell -c` by name.
    fn exec(&mut self, channel: ChannelId, line: &[u8]) {
        let argv = std::str::from_utf8(line)
            .map_err(|_| "the command is not UTF-8".to_string())
            .and_then(command::split)
            .and_then(|argv| Ok(std::iter::once(command::resolve(&argv[0])?).chain(argv.into_iter().skip(1)).collect()));
        match argv {
            Ok(argv) => self.start(channel, argv, false),
            Err(why) => self.refuse(channel, &why),
        }
    }

    /// End `channel` with `why` on its stderr and [`EXIT_REFUSED`].
    fn refuse(&mut self, channel: ChannelId, why: &str) {
        println!("sshserver: {}: refused: {why}", self.peer);
        let _ = self.server.send_stderr(channel, format!("sshserver: {why}\r\n").as_bytes());
        let _ = self.server.exit(channel, EXIT_REFUSED);
    }

    fn start(&mut self, channel: ChannelId, argv: Vec<String>, crlf: bool) {
        let spawned = Command::new(&argv[0]).args(&argv[1..]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => return self.refuse(channel, &format!("cannot run {}: {e}", argv[0])),
        };
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let program = Arc::new(Program::new(child));
        let (input, queue) = mpsc::channel();
        let tx = self.tx.clone();
        std::thread::spawn(move || write_stdin(stdin, queue, channel, tx));
        pump(stdout, false, channel, self.tx.clone());
        pump(stderr, true, channel, self.tx.clone());
        let name = argv.into_iter().next().expect("a program");
        let running = Running { input: Some(input), output: VecDeque::new(), streams: 2, status: None, crlf, program: Some((program, name)) };
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
        self.channels.insert(channel, Running { input: Some(input), output: VecDeque::new(), streams: 1, status: None, crlf: false, program: None });
    }

    /// Hand `channel`'s waiting output to the client, as much as its window
    /// takes, and tell each reader whose chunk went.
    fn flush(&mut self, channel: ChannelId) {
        let Some(running) = self.channels.get_mut(&channel) else { return };
        while let Some(front) = running.output.front_mut() {
            let rest = &front.data[front.sent..];
            let sent = match front.stderr {
                false => self.server.send(channel, rest),
                true => self.server.send_stderr(channel, rest),
            };
            match sent {
                Ok(n) if n == rest.len() => {
                    let done = running.output.pop_front().expect("the front");
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

    /// Say how the program ended once both its streams have: a reader ends
    /// only after its last chunk was taken, so all its output is out.
    fn finish(&mut self, channel: ChannelId) {
        let Some(running) = self.channels.get(&channel) else { return };
        if let (0, Some(status)) = (running.streams, running.status) {
            debug_assert!(running.output.is_empty(), "a stream ended with its output still waiting");
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

/// End what runs on a channel: its program, whose pipes then end its readers.
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
    #[test]
    fn a_terminal_gets_a_carriage_return_before_every_line_feed() {
        assert_eq!(super::crlf(b"a\nb\n\nc"), b"a\r\nb\r\n\r\nc");
        assert_eq!(super::crlf(b""), b"");
    }
}
