//! Login sessions: each one's owner, `/system/bin/session`, which the
//! supervisor starts as its child, and what the supervisor does for it.
//!
//! **The supervisor opens a session for a `login` row**: for the screen's row
//! as it starts it, endowing that session's launcher and a control connection
//! ([`Desktop`]), and for any `login` row that asks on [`wire::LOGIN_PORT`].
//! Each session has a launcher port of its own; the supervisor mints its
//! badges and the owner accepts on it, handing each connection back over the
//! owner's link with its badge ([`wire::MSG_HANDOVER`]). **The session is the
//! link's, never the badge's or the request's.**
//!
//! **Every app started in a session is the supervisor's to watch** ([`Watch`]):
//! it tells the owner of each start, with a handle that may quit and kill it,
//! and of each end. A detached app is placed under the owner, so the owner's
//! end takes the session's apps down.
//!
//! **A session ends with its owner.** Ended by exit 0, the owner signed it out;
//! any other end is a failure, after which its login authority's next open
//! spends a budget ([`Reopens`]).

use std::process::{Child, Command};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;

use toyos::ipc::{self, Connection, RxStep};
use toyos::launch::LAUNCHER;
use toyos::namespace::{self, Namespace};
use toyos::poller::{Poller, READABLE};
use toyos::port::{self, Acceptor};
use toyos::{say, AsHandle, OwnedHandle, Pipe, RawHandle};
use toyos_abi::syscall;
use toyos_abi::Rights;
use toyos_manifest::launch::{Authority, Screen, Session};
use toyos_manifest::Program;
use toyos_session::{self as wire, Listed, Names, SignOut, SignOutSession, Started, State};
use toyos_supervisor::sessions::{self as decide, Reopens};

use crate::{exit_of, start, Caller, Extra, LaunchRx, Minting, Output, Pending, Port, Served, Supervisor};

/// The name a session's controls namespace holds its control port under.
const CONTROL: &str = "control";

/// The login sessions the supervisor watches the owners of at once. Each
/// owner is a process its loop watches, beside its services and `drives`
/// processes. The last is the desktop's alone, so sessions left ending by an
/// app that ignores its hang-up never keep the desktop from opening again.
pub(crate) const MAX_SESSIONS: usize = 16;

/// One open login session.
pub(crate) struct Login {
    pub session: Session,
    /// The login authority's row that opened it.
    opener: &'static Program,
    pub owner: Child,
    /// The supervisor's end of the owner's link.
    pub link: Connection,
    rx: LaunchRx,
    /// The owner's handle to itself, which places a detached app under it.
    place: Option<OwnedHandle>,
    /// The session's launcher port, which every launcher in it is minted on.
    /// Held here alone, so the session's end closes it and a launch connected
    /// to it reads its end.
    pub launcher: Acceptor,
    /// A connector to the session's control port, for each control connection
    /// a login authority is handed.
    controls: Namespace,
    /// Whether the screen's row opened it: the desktop's.
    desktop: bool,
    state: State,
    waiting: Names,
}

/// The desktop session's launcher and a control connection to it, minted for
/// one start of the screen's row.
pub(crate) struct Desktop {
    pub launcher: Namespace,
    pub control: Connection,
}

impl Login {
    /// `opener`'s launcher in this session, and a control connection to it.
    fn handed_to(&self, opener: &Program) -> Result<(Namespace, Connection), String> {
        let badge = Authority { row: opener.name.clone(), session: self.session }.encode();
        let connector = self.launcher.mint(&badge).map_err(|e| format!("no launcher was minted ({e:?})"))?;
        let launcher = namespace::build().add(LAUNCHER, &connector).finish().map_err(|e| format!("{e:?}"))?;
        let control = self.controls.open(CONTROL).map_err(|e| format!("no control connection ({e:?})"))?;
        Ok((launcher, control))
    }
}

#[derive(Default)]
pub(crate) struct Logins {
    pub open: Vec<Login>,
    /// The next app's instance: never reused in a boot.
    next_instance: u64,
    reopens: Reopens,
}

impl Logins {
    fn of(&self, session: Session) -> Option<&Login> {
        self.open.iter().find(|login| login.session == session)
    }

    /// Where an app detached in `session` is placed: under that session's
    /// owner, `Ok(None)` in the machine's session, which is under the
    /// supervisor, and `Err` in a login session whose owner has given no place.
    pub fn detached_place(&self, session: Session) -> Result<Option<RawHandle>, &'static str> {
        let place = self.of(session).and_then(|login| login.place.as_ref());
        match decide::detached(matches!(session, Session::Login(..)), place.is_some()) {
            decide::Placed::UnderSupervisor => Ok(None),
            decide::Placed::UnderOwner => Ok(place.map(|place| place.as_handle())),
            decide::Placed::Unplaced => Err("its session's owner gave no place to detach under"),
        }
    }

    /// The launcher port `session`'s launches are minted on, while it is open.
    pub fn launcher_of(&self, session: Session) -> Option<&Acceptor> {
        self.of(session).map(|login| &login.launcher)
    }
}

/// The ends of every app started in a login session, watched off the loop.
///
/// **One thread per [`PER_WATCHER`] apps**, each waiting on its own poller, so
/// the set is bounded by the process table and not by one poller's width. An
/// end reaches the loop through one pipe, which it watches under
/// `TOKEN_INSTANCES`.
pub(crate) struct Watch {
    /// Readable once per end.
    pub ended: Pipe,
    notify: Arc<Pipe>,
    ends: mpsc::Receiver<(u64, Session)>,
    sender: mpsc::Sender<(u64, Session)>,
    watchers: Vec<Watcher>,
}

/// What one watcher thread's poller holds beside its wake pipe.
const PER_WATCHER: usize = Poller::MAX_HANDLES as usize - 1;
const WAKE: u64 = u64::MAX;

struct Watcher {
    add: mpsc::Sender<(u64, Session, OwnedHandle)>,
    wake: Pipe,
    held: Arc<AtomicUsize>,
}

impl Watch {
    pub fn new() -> Self {
        let (ended, notify) = toyos::pipe_pair().expect("supervisor: no pipe for its apps' ends");
        let (sender, ends) = mpsc::channel();
        Self { ended, notify: Arc::new(notify), ends, sender, watchers: Vec::new() }
    }

    /// Watch app `instance` of `session` until it ends.
    fn add(&mut self, instance: u64, session: Session, handle: OwnedHandle) {
        let at = match self.watchers.iter().position(|w| w.held.load(Ordering::Relaxed) < PER_WATCHER) {
            Some(at) => at,
            None => {
                self.watchers.push(self.watcher());
                self.watchers.len() - 1
            }
        };
        let watcher = &self.watchers[at];
        watcher.held.fetch_add(1, Ordering::Relaxed);
        watcher.add.send((instance, session, handle)).expect("supervisor: a watcher of its apps has ended");
        let _ = watcher.wake.write_nonblock(&[1]);
    }

    fn watcher(&self) -> Watcher {
        let (add, added) = mpsc::channel::<(u64, Session, OwnedHandle)>();
        let (woken, wake) = toyos::pipe_pair().expect("supervisor: no pipe for a watcher of its apps");
        let held = Arc::new(AtomicUsize::new(0));
        let (notify, sender, count) = (Arc::clone(&self.notify), self.sender.clone(), Arc::clone(&held));
        std::thread::Builder::new()
            .name("apps".into())
            .spawn(move || {
                let poller = Poller::new(Poller::MAX_HANDLES);
                let mut apps: Vec<(u64, Session, OwnedHandle)> = Vec::new();
                let mut ready = Vec::new();
                loop {
                    apps.extend(added.try_iter());
                    poller.watch(&woken, READABLE, WAKE);
                    for (i, (_, _, handle)) in apps.iter().enumerate() {
                        poller.watch_raw(handle.as_handle(), READABLE, i as u64);
                    }
                    ready.clear();
                    poller.wait(1, u64::MAX, |token| ready.push(token));
                    if ready.contains(&WAKE) {
                        let mut drained = [0u8; 64];
                        while woken.read_nonblock(&mut drained).is_ok_and(|n| n > 0) {}
                    }
                    let mut at = apps.len();
                    while at > 0 {
                        at -= 1;
                        if syscall::process_wait_nonblock(apps[at].2.as_handle()).is_err() {
                            continue;
                        }
                        let (instance, session, _) = apps.swap_remove(at);
                        count.fetch_sub(1, Ordering::Relaxed);
                        sender.send((instance, session)).expect("supervisor: its loop has ended");
                        let _ = notify.write_nonblock(&[1]);
                    }
                }
            })
            .expect("supervisor: a watcher of its apps could not be started");
        Watcher { add, wake, held }
    }
}

impl Supervisor<'_> {
    /// Open a login session for `opener` and start its owner: the index of
    /// the session, or why none was opened.
    fn open_session(&mut self, opener: &'static Program) -> Result<usize, String> {
        let now = Instant::now();
        self.logins.reopens.may_open(&opener.name, now).map_err(|_| {
            format!(
                "more than {} of its sessions failed in {} s",
                toyos_supervisor::restart::RESTARTS,
                toyos_supervisor::restart::WINDOW.as_secs()
            )
        })?;
        let desktop = Screen::of(opener) == Screen::Held;
        let room = if desktop { MAX_SESSIONS } else { MAX_SESSIONS - 1 };
        if self.logins.open.len() >= room {
            return Err(format!("{} sessions are open", self.logins.open.len()));
        }
        let owner_row = self
            .system
            .program(wire::OWNER)
            .ok_or_else(|| format!("this image declares no `{}` row to own a session", wire::OWNER))?;
        let path = owner_row.source.path().ok_or("the session owner's row runs no file")?;
        let session = self.sessions.open(Screen::of(opener));
        let (launcher, _) = port::create().map_err(|e| format!("no launcher port ({e:?})"))?;
        let (controls_acceptor, controls_connector) = port::create().map_err(|e| format!("no control port ({e:?})"))?;
        let (link_acceptor, link_connector) = port::create().map_err(|e| format!("no link ({e:?})"))?;
        let links = namespace::build().add(CONTROL, &link_connector).finish().map_err(|e| format!("{e:?}"))?;
        let link = links.open(CONTROL).map_err(|e| format!("the link would not open ({e:?})"))?;
        let owner_end = link_acceptor.accept().map_err(|e| format!("the link was not accepted ({e:?})"))?;
        let controls = namespace::build().add(CONTROL, &controls_connector).finish().map_err(|e| format!("{e:?}"))?;
        let accepting = syscall::dup(launcher.as_handle()).map_err(|e| format!("no copy of its launcher ({e:?})"))?;
        let extra = Extra {
            endow: vec![
                (wire::LINK_LABEL, owner_end.into_raw()),
                (wire::LAUNCHER_ACCEPTOR_LABEL, accepting),
                (wire::CONTROL_ACCEPTOR_LABEL, controls_acceptor.into_raw()),
            ],
            ..Extra::default()
        };
        self.make_state_dir(owner_row);
        let minting = Minting { minted: &self.minted, session, launches: None };
        let served = Served::Keep { acceptors: &[], owed: &[] };
        let (owner, _) = start(
            Command::new(path),
            owner_row,
            self.system,
            self.syscap,
            served,
            &self.connectors,
            &self.grants,
            &[],
            extra,
            minting,
            Output::Boot(&mut self.log),
        )
        .map_err(|e| format!("its owner did not start: {e}"))?;
        say!("supervisor: session {} opened for {}, owned by pid {}", session.share(), opener.name, owner.id());
        self.logins.open.push(Login {
            session,
            opener,
            owner,
            link,
            rx: LaunchRx::new(),
            place: None,
            launcher,
            controls,
            desktop,
            state: State::Active,
            waiting: Names::default(),
        });
        Ok(self.logins.open.len() - 1)
    }

    /// The desktop session the screen's row at `index` starts holding,
    /// minted for that start: the screen row's open session, or one opened
    /// for it now. A start with none holds no launcher, and the compositor
    /// says so at each launch it is asked for.
    pub(crate) fn desktop_for(&mut self, index: usize) {
        let program = self.services[index].program;
        if !(program.login && Screen::of(program) == Screen::Held) {
            return;
        }
        let program = self.system.program(&program.name).expect("a `login` row is the manifest's");
        let open = self.logins.open.iter().position(|l| l.desktop && l.opener.name == program.name);
        let at = match open.map_or_else(|| self.open_session(program), Ok) {
            Ok(at) => at,
            Err(why) => return say!("supervisor: {} starts with no desktop session: {why}", program.name),
        };
        self.services[index].desktop = match self.logins.open[at].handed_to(program) {
            Ok((launcher, control)) => Some(Desktop { launcher, control }),
            Err(why) => {
                say!("supervisor: {} starts with no desktop session: {why}", program.name);
                None
            }
        };
    }

    /// One request on [`wire::LOGIN_PORT`] from `opener`'s row.
    pub(crate) fn serve_login(&mut self, conn: &Connection, opener: &'static Program, msg_type: u32) {
        if msg_type != wire::MSG_OPEN {
            say!("supervisor: login: {} asked {msg_type}, which is no request", opener.name);
            return;
        }
        // The screen's row is answered its open desktop session, as
        // `desktop_for` is: there is one desktop session at a time.
        let open = (Screen::of(opener) == Screen::Held)
            .then(|| self.logins.open.iter().position(|l| l.desktop && l.opener.name == opener.name))
            .flatten();
        let answered = open
            .map_or_else(|| self.open_session(opener), Ok)
            .and_then(|at| self.logins.open[at].handed_to(opener));
        match answered {
            Ok((ns, control)) => {
                // SAFETY: the namespace's own handle, given up here and to nothing else.
                let handles = [unsafe { OwnedHandle::from_raw(ns.into_raw()) }, control.into_owned()];
                if let Err(e) = conn.try_signal_with(wire::MSG_OPENED, handles) {
                    say!("supervisor: login: {}'s session was opened and not answered ({e:?})", opener.name);
                }
            }
            Err(why) => {
                say!("supervisor: login: {} was refused a session: {why}", opener.name);
                let _ = conn.try_send_bytes(wire::MSG_REFUSED, why.as_bytes());
            }
        }
    }

    /// Everything each session owner said on its link: its launcher's
    /// connections, answered as launches in its session, and its reports.
    pub(crate) fn pump_links(&mut self, pending: &mut Vec<Pending>) {
        for at in 0..self.logins.open.len() {
            loop {
                let login = &mut self.logins.open[at];
                let (msg_type, payload, batch) = match login.rx.pump(&login.link) {
                    RxStep::Idle | RxStep::Eof => break,
                    // The supervisor's own child broke its protocol: the session
                    // ends with it, as a failure.
                    RxStep::Malformed => {
                        say!("supervisor: session {}'s owner sent a frame its link cannot describe; it is ended", login.session.share());
                        let _ = login.owner.kill();
                        break;
                    }
                    RxStep::Frame { msg_type, payload_len, .. } => {
                        (msg_type, login.rx.payload(payload_len).to_vec(), login.rx.take_batch())
                    }
                };
                let session = login.session;
                match msg_type {
                    wire::MSG_HANDOVER => match self.handed_over(session, &payload, batch) {
                        Ok(handed) if pending.len() < crate::MAX_PENDING_LAUNCHES => pending.push(handed),
                        Ok(_) => say!("supervisor: launcher: refusing a handed-over launch: the table is full"),
                        Err(why) => say!("supervisor: launcher: a launch session {} handed over is dropped: {why}", session.share()),
                    },
                    wire::MSG_PLACE => match batch.exact::<1>() {
                        Some([handle]) => {
                            // SAFETY: moved into this table with the frame, and nothing else answers for it.
                            self.logins.open[at].place = Some(unsafe { OwnedHandle::from_raw(handle) });
                        }
                        None => say!("supervisor: session {}'s owner sent no place", session.share()),
                    },
                    wire::MSG_WAITING => {
                        let login = &mut self.logins.open[at];
                        login.state = State::Ending;
                        login.waiting = Names::decode(&payload).unwrap_or_default();
                    }
                    wire::MSG_CANCELLED => {
                        let login = &mut self.logins.open[at];
                        login.state = State::Active;
                        login.waiting = Names::default();
                    }
                    other => say!("supervisor: session {}'s owner said {other}, which its link has no word for", session.share()),
                }
            }
        }
    }

    /// A connection `session`'s owner accepted on its launcher, as a launch
    /// waiting for its frame. Its caller is the badge's row; its session is
    /// `session`, which the badge must name.
    fn handed_over(&self, session: Session, badge: &[u8], batch: ipc::Batch) -> Result<Pending, String> {
        let [handle] = batch.exact::<1>().ok_or("it carried no single connection")?;
        let conn = Connection::received(handle);
        let Some(Authority { row, session: named }) = Authority::decode(badge) else {
            return Err(format!("its badge is not one this supervisor writes: {badge:?}"));
        };
        let session = decide::handed_over(session, named).map_err(|named| format!("its badge names session {}", named.share()))?;
        let row = self.system.program(&row).ok_or_else(|| format!("its badge names `{row}`, which is no row"))?;
        Ok(Pending { conn, rx: LaunchRx::new(), since: Instant::now(), port: Port::Launcher(Caller { row, session }) })
    }

    /// An app started in a login session: watched, and its owner told.
    pub(crate) fn started_in(&mut self, session: Session, name: &str, process: RawHandle) {
        let Some(login) = self.logins.open.iter().find(|login| login.session == session) else { return };
        let instance = self.logins.next_instance;
        self.logins.next_instance += 1;
        let watched = syscall::dup_narrowed(process, Rights::WAIT);
        let managed = syscall::dup_narrowed(process, Rights::MANAGE.union(Rights::TRANSFER));
        let (Ok(watched), Ok(managed)) = (watched, managed) else {
            return say!("supervisor: {name} in session {} was started and is not watched", session.share());
        };
        let started = Started { instance, name: name.to_string() };
        // SAFETY: a duplicate made above, which nothing else answers for.
        let managed = unsafe { OwnedHandle::from_raw(managed) };
        if let Err(e) = login.link.try_send_bytes_with(wire::MSG_STARTED, &started.encode(), [managed]) {
            say!("supervisor: session {}'s owner was not told of {name} ({e:?})", session.share());
        }
        // SAFETY: as above.
        self.watch.add(instance, session, unsafe { OwnedHandle::from_raw(watched) });
    }

    /// Every app end the watch saw, told to its session's owner.
    pub(crate) fn apps_ended(&mut self) {
        let mut drained = [0u8; 64];
        while self.watch.ended.read_nonblock(&mut drained).is_ok_and(|n| n > 0) {}
        while let Ok((instance, session)) = self.watch.ends.try_recv() {
            if let Some(login) = self.logins.of(session) {
                let _ = login.link.try_send_bytes(wire::MSG_ENDED, &instance.to_le_bytes());
            }
        }
    }

    /// The owners the loop watches the ends of.
    pub(crate) fn owners(&self) -> impl Iterator<Item = &Child> {
        self.logins.open.iter().map(|login| &login.owner)
    }

    /// Every session whose owner has ended, closed: its launcher's port with
    /// it, and a failure counted against its login authority.
    pub(crate) fn sessions_ended(&mut self) {
        let mut at = 0;
        while at < self.logins.open.len() {
            let Some(end) = exit_of(&self.logins.open[at].owner) else {
                at += 1;
                continue;
            };
            let login = self.logins.open.swap_remove(at);
            let signed_out = decide::signed_out(end);
            self.logins.reopens.ended(&login.opener.name, signed_out, Instant::now());
            match signed_out {
                true => say!("supervisor: session {} is signed out", login.session.share()),
                false => say!("supervisor: session {} ended with its owner, {end}", login.session.share()),
            }
        }
    }

    /// One request on [`wire::ADMIN_PORT`], from `logout`.
    pub(crate) fn serve_admin(&mut self, conn: &Connection, caller: &Caller, msg_type: u32, payload: &[u8]) {
        let answer = match caller.session {
            Session::Machine(_) => Err("only a login session's `logout` acts on sessions".to_string()),
            Session::Login(..) => self.admin(msg_type, payload),
        };
        let (msg, text) = match answer {
            Ok(text) => (if msg_type == wire::MSG_LIST { wire::MSG_SESSIONS } else { wire::MSG_DONE }, text),
            Err(why) => {
                say!("supervisor: session-admin: refused {} in {}: {why}", caller.row.name, caller.session);
                (wire::MSG_REFUSED, why)
            }
        };
        let _ = conn.try_send_bytes(msg, text.as_bytes());
    }

    /// **The machine has one user**, so a login session's `logout` acts on
    /// every session open, its own and any other.
    fn admin(&mut self, msg_type: u32, payload: &[u8]) -> Result<String, String> {
        match msg_type {
            wire::MSG_LIST => {
                let lines: Vec<String> = self
                    .logins
                    .open
                    .iter()
                    .map(|l| Listed {
                        share: l.session.share(),
                        desktop: l.session.at_screen(),
                        state: l.state,
                        waiting: l.waiting.clone(),
                    })
                    .map(|listed| listed.line())
                    .collect();
                Ok(lines.join("\n"))
            }
            wire::MSG_SIGN_OUT_SESSION => {
                let asked = SignOutSession::decode(payload).ok_or("its request is not one `logout` writes")?;
                let login = self
                    .logins
                    .open
                    .iter()
                    .find(|l| l.session.share() == asked.share)
                    .ok_or_else(|| format!("no session {} is open", asked.share))?;
                let SignOut { force } = asked.sign_out;
                login
                    .link
                    .try_send_bytes(wire::MSG_SIGN_OUT, &asked.sign_out.encode())
                    .map_err(|e| format!("session {}'s owner was not asked ({e:?})", asked.share))?;
                Ok(format!("session {} is asked to sign out{}", asked.share, if force { ", by force" } else { "" }))
            }
            other => Err(format!("{other} is no request")),
        }
    }
}
