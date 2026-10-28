//! The desktop session this compositor is the login authority of: its control
//! connection, which signs the session out, and the thread its launches and
//! its opens of a session are made on.
//!
//! The supervisor starts this row holding the desktop session's launcher and
//! its control connection, and no other launcher: its launches are that
//! session's alone. **A session that ends is opened again** on the `login`
//! port, which the supervisor refuses only past a budget of failures: a
//! session signed out comes back empty, there being no greeter yet. While none
//! is open, each launch asked for asks for one first, and a refused open says
//! why and its launch starts nothing. **Every open is the launcher thread's**,
//! a supervisor round trip the loop never waits on: the control connection
//! it opens comes back to the loop through [`Desktop::opened`].
//!
//! **A sign-out the session's owner says is the machine's stop**
//! ([`wire::MSG_STOPPING`]) is decided on `power`: its cancel and force are
//! the stop's, sent on the launcher thread too.

use std::os::toyos::process::CommandExt;
use std::process::Command;
use std::sync::mpsc;

use toyos::endow::{self, Endowments};
use toyos::ipc::{self, Connection, RxStep};
use toyos::job::JOB_LABEL;
use toyos::namespace::Namespace;
use toyos::syscap::SysCap;
use toyos::wake::{self, Bell, Waker};
use toyos::AsHandle;
use toyos_abi::syscall::SYSCAP_LABEL;
use toyos_desktop::Opening;
use toyos_session::{self as wire, Names, SignOut};

/// What the launcher thread is asked.
enum Ask {
    /// Open a desktop session on the `login` port.
    Open,
    Launch(String),
    /// Cancel or force the machine's stop.
    Stop(Decide),
}

#[derive(Clone, Copy, Debug)]
enum Decide {
    Cancel,
    Terminate,
}

pub struct Desktop {
    control: Option<Connection>,
    rx: ipc::FrameRx<{ ipc::MAX_FRAME_LEN as usize }>,
    /// What a sign-out waits for, while one does.
    pub waiting: Option<Names>,
    /// The sign-out is the machine's stop.
    pub stopping: bool,
    asks: mpsc::Sender<Ask>,
    /// The answers to the thread's opens: a control connection, or why none.
    opened: mpsc::Receiver<Result<Connection, String>>,
    /// Readable once per answer in `opened`.
    wake: Bell,
    opening: Opening,
}

impl Desktop {
    /// The endowed session, and the thread launches and opens are made on.
    pub fn start() -> Self {
        let endowed = Endowments::get();
        let (asks, queue) = mpsc::channel();
        let (sent, opened) = mpsc::channel();
        let (woken, wake) = wake::pair().expect("compositor: no event for its session's opens");
        let login = endowed.take::<Namespace>(wire::LOGIN_PORT);
        std::thread::Builder::new()
            .name("launcher".into())
            .spawn(move || launcher(queue, login, sent, woken))
            .expect("compositor: no thread to launch on");
        // Connected here, so the control connection is the compositor's own.
        let control = endowed.take::<Namespace>(wire::CONTROL_LABEL).and_then(|controls| {
            controls.open(wire::CONTROL).inspect_err(|why| eprintln!("compositor: its session's controls refused it: {why}")).ok()
        });
        let mut desktop = Self {
            control,
            rx: ipc::FrameRx::new(),
            waiting: None,
            stopping: false,
            asks,
            opened,
            wake,
            opening: Opening::default(),
        };
        if desktop.control.is_none() {
            eprintln!("compositor: started with no desktop session");
            desktop.ask();
        }
        desktop
    }

    pub fn conn(&self) -> Option<&Connection> {
        self.control.as_ref()
    }

    /// What the loop watches for an open made.
    pub fn wake(&self) -> &Bell {
        &self.wake
    }

    /// Start `program` in the desktop session, off the loop, asking for a
    /// session first where none is open.
    pub fn launch(&mut self, program: &str) {
        self.ask();
        self.asks.send(Ask::Launch(program.to_string())).expect("compositor: its launcher thread has ended");
    }

    /// Ask the thread for a session, where none is open and none is asked.
    fn ask(&mut self) {
        if self.opening.ask(self.control.is_some()) {
            self.asks.send(Ask::Open).expect("compositor: its launcher thread has ended");
        }
    }

    /// A session the thread opened, now the one this compositor signs out:
    /// `true` where there is one.
    pub fn opened(&mut self) -> bool {
        // Its one waker is the launcher thread's, which ends only with this.
        self.wake.try_take().expect("compositor: its launcher thread has ended");
        let mut took = false;
        while let Ok(answer) = self.opened.try_recv() {
            self.opening.answered();
            match answer {
                Ok(control) => {
                    println!("compositor: a desktop session is open again");
                    self.control = Some(control);
                    self.rx = ipc::FrameRx::new();
                    took = true;
                }
                Err(why) => eprintln!("compositor: no desktop session: {why}; the next launch asks again"),
            }
        }
        took
    }

    /// Sign the session out: ask what runs in it to quit, or end it by `force`.
    pub fn sign_out(&mut self, force: bool) {
        if self.stopping {
            if force {
                self.decide(Decide::Terminate);
            }
            return;
        }
        let Some(control) = &self.control else { return };
        if let Err(e) = control.try_send_bytes(wire::MSG_SIGN_OUT, &SignOut { force }.encode()) {
            return eprintln!("compositor: the sign-out was not asked: {e:?}");
        }
        println!("compositor: signing out{}", if force { ", ending what still runs" } else { "" });
        self.waiting.get_or_insert_with(Names::default);
    }

    pub fn cancel(&mut self) {
        if self.stopping {
            return self.decide(Decide::Cancel);
        }
        let Some(control) = &self.control else { return };
        if let Err(e) = control.try_signal(wire::MSG_CANCEL) {
            eprintln!("compositor: the cancel was not sent: {e:?}");
        }
    }

    fn decide(&mut self, decide: Decide) {
        self.asks.send(Ask::Stop(decide)).expect("compositor: its launcher thread has ended");
    }

    /// Everything the session's owner said; `true` where the view changed.
    pub fn said(&mut self) -> bool {
        let mut changed = false;
        loop {
            let Some(control) = &self.control else { return changed };
            match self.rx.pump(control) {
                RxStep::Idle => return changed,
                RxStep::Frame { msg_type, payload_len, .. } => {
                    let payload = self.rx.payload(payload_len);
                    match (msg_type, Names::decode(payload)) {
                        (wire::MSG_WAITING, Some(names)) => {
                            println!("compositor: signing out, waiting for: {}", names.names.join(", "));
                            self.waiting = Some(names);
                        }
                        (wire::MSG_STOPPING, _) if payload == [1] => {
                            println!("compositor: the machine stops; Esc cancels the stop, Enter forces it");
                            self.stopping = true;
                            self.waiting.get_or_insert_with(Names::default);
                        }
                        (wire::MSG_STOPPING, _) if payload == [0] => {
                            println!("compositor: the machine's stop is cancelled; the sign-out goes on");
                            self.stopping = false;
                        }
                        (wire::MSG_CANCELLED, _) => {
                            println!("compositor: the sign-out is cancelled; the session goes on");
                            self.waiting = None;
                            self.stopping = false;
                        }
                        _ => eprintln!("compositor: the session's owner said {msg_type}, which is no report"),
                    }
                    changed = true;
                }
                RxStep::Eof | RxStep::Malformed => {
                    println!("compositor: the desktop session ended");
                    self.control = None;
                    self.waiting = None;
                    self.stopping = false;
                    self.ask();
                    return true;
                }
            }
        }
    }
}

/// The thread every launch and every open of a session is made on, in the
/// order asked: a launch waits as long as its question does, and an open is a
/// round trip to the supervisor.
///
/// **Everything the desktop starts is a job of its own**, made with the
/// `jobs` right this row's system capability carries: a program started from
/// the taskbar is no helper of the compositor's, and must not share its CPU.
fn launcher(queue: mpsc::Receiver<Ask>, login: Option<Namespace>, opened: mpsc::Sender<Result<Connection, String>>, wake: Waker) {
    let jobs = endow::Endowments::get().take::<SysCap>(SYSCAP_LABEL);
    // A session this thread opened, whose launcher replaces the endowed one.
    let mut session: Option<Namespace> = None;
    for ask in queue {
        let program = match ask {
            // Answered either way, so the loop asks again after a refusal.
            Ask::Open => {
                let answer = match &login {
                    Some(login) => wire::open(login).map(|(launcher, control)| {
                        session = Some(launcher);
                        control
                    }),
                    None => Err("this row holds no `login` connector".to_string()),
                };
                if opened.send(answer).is_ok() {
                    wake.wake();
                }
                continue;
            }
            Ask::Stop(decide) => {
                let decided = match decide {
                    Decide::Cancel => toyos::power::cancel(),
                    Decide::Terminate => toyos::power::terminate(),
                };
                if let Err(why) = decided {
                    eprintln!("compositor: the stop's {decide:?} was refused: {why:?}");
                }
                continue;
            }
            Ask::Launch(program) => program,
        };
        let Some(jobs) = &jobs else {
            eprintln!("compositor: {program} did not start: this row holds no `jobs` right to start it as its own job");
            continue;
        };
        let job = match jobs.new_job() {
            Ok(job) => job,
            Err(e) => {
                eprintln!("compositor: {program} did not start: no job for it: {e:?}");
                continue;
            }
        };
        let mut command = Command::new(&program);
        command.independent().detached().endow(JOB_LABEL, job.into());
        if let Some(launcher) = &session {
            command.launcher(launcher.as_handle().into());
        }
        if let Err(e) = command.spawn() {
            eprintln!("compositor: {program} did not start: {e}");
        }
    }
}
