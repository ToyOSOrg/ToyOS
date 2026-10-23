//! The desktop session this compositor is the login authority of: its control
//! connection, which signs the session out, and the launcher its launches go
//! through.
//!
//! The supervisor starts this row holding the desktop session's launcher and
//! its control connection. **A session that ends is opened again** on the
//! `login` port, which the supervisor budgets only after a failure: a
//! session signed out comes back empty, there being no greeter yet, and one
//! whose owner failed comes back unless its failures are past the budget.
//! Until then a launch goes nowhere, and says so.

use std::sync::{Arc, Mutex};

use toyos::endow::Endowments;
use toyos::ipc::{self, Connection, RxStep};
use toyos::namespace::Namespace;
use toyos_session::{self as wire, Names, SignOut};

/// The launcher a launch goes through: the endowed one while `None`, and
/// one this compositor opened a session for once that one ended.
pub type Launcher = Arc<Mutex<Option<Namespace>>>;

pub struct Desktop {
    control: Option<Connection>,
    rx: ipc::FrameRx<{ ipc::MAX_FRAME_LEN as usize }>,
    /// What a sign-out waits for, while one does.
    pub waiting: Option<Names>,
    login: Option<Namespace>,
    pub launcher: Launcher,
}

impl Desktop {
    pub fn take() -> Self {
        let endowed = Endowments::get();
        let control = endowed.take::<Connection>(wire::CONTROL_LABEL);
        if control.is_none() {
            eprintln!("compositor: started with no desktop session; its launches go nowhere");
        }
        Self {
            control,
            rx: ipc::FrameRx::new(),
            waiting: None,
            login: endowed.take(wire::LOGIN_PORT),
            launcher: Arc::new(Mutex::new(None)),
        }
    }

    pub fn conn(&self) -> Option<&Connection> {
        self.control.as_ref()
    }

    /// Sign the session out: ask what runs in it to quit, or end it by `force`.
    pub fn sign_out(&mut self, force: bool) {
        let Some(control) = &self.control else { return };
        if let Err(e) = control.try_send_bytes(wire::MSG_SIGN_OUT, &SignOut { force }.encode()) {
            return eprintln!("compositor: the sign-out was not asked: {e:?}");
        }
        println!("compositor: signing out{}", if force { ", ending what still runs" } else { "" });
        self.waiting.get_or_insert_with(Names::default);
    }

    pub fn cancel(&mut self) {
        let Some(control) = &self.control else { return };
        if let Err(e) = control.try_signal(wire::MSG_CANCEL) {
            eprintln!("compositor: the cancel was not sent: {e:?}");
        }
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
                        (wire::MSG_CANCELLED, _) => {
                            println!("compositor: the sign-out is cancelled; the session goes on");
                            self.waiting = None;
                        }
                        _ => eprintln!("compositor: the session's owner said {msg_type}, which is no report"),
                    }
                    changed = true;
                }
                RxStep::Eof | RxStep::Malformed => {
                    println!("compositor: the desktop session ended");
                    self.control = None;
                    self.waiting = None;
                    self.reopen();
                    return true;
                }
            }
        }
    }

    /// A session of this compositor's own, opened on the `login` port.
    fn reopen(&mut self) {
        let Some(login) = &self.login else {
            return eprintln!("compositor: no `login` connector to open a desktop session with");
        };
        match wire::open(login) {
            Ok((launcher, control)) => {
                println!("compositor: a desktop session is open again");
                *self.launcher.lock().expect("compositor: its launcher lock is whole") = Some(launcher);
                self.control = Some(control);
                self.rx = ipc::FrameRx::new();
            }
            Err(why) => eprintln!("compositor: no desktop session: {why}"),
        }
    }
}
