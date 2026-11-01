//! A login session, end to end, from a job test-runner hands its `login`
//! connector, as a login authority: the session's owner parents what is
//! detached in it, signs it out by asking and waiting, and ends what stays
//! only when forced; `logout` lists it; and opens are never budgeted.
//! Last, the machine's reboot waits for what a session runs until it is
//! forced, a cancel keeping both, and a terminate reboots the machine, which
//! ends the job. `hangupholder` is the app that ignores every quit.

use std::os::toyos::process::CommandExt;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::ipc::{self, Connection, RxStep};
use toyos::namespace::Namespace;
use toyos::poller::{Poller, READABLE};
use toyos::power::{self, Mode, Pending, Stop, Update};
use toyos::syscap::SysCap;
use toyos::AsHandle;
use toyos_session::{self as wire, Names, SignOut};

const HOLDER: &str = "hangupholder";
const ZOMBIE: u8 = 3;
/// Opens after one another, none of them after a failure.
const OPENS: usize = 10;
/// A hang ceiling, never a verdict on time: QEMU's clock is no measure.
const CEILING: Duration = Duration::from_secs(60);

fn main() {
    let login = Endowments::get().take::<Namespace>(wire::LOGIN_PORT).expect("test-runner hands this job its `login`");
    let (launcher, control) = wire::open(&login).expect("a session for this job");
    let mut control = Control { conn: control, rx: ipc::FrameRx::new() };
    // `logout` runs in a second session, which no sign-out here asks to quit.
    let (admin, _admin_control) = wire::open(&login).expect("a second session, for `logout`");
    let launched = |launcher: &Namespace, program: &str, args: &[&str]| {
        Command::new(program)
            .independent()
            .launcher(launcher.as_handle().into())
            .args(args)
            .current_dir("/")
            .output()
            .unwrap_or_else(|e| panic!("launch {program} in a session: {e}"))
    };
    let logout = || String::from_utf8_lossy(&launched(&admin, "/system/bin/logout", &[]).stdout).into_owned();

    // An app detached in the session runs there, listening for its quits.
    let holder = detach(&launcher);
    println!("session_lifecycle: a detached app (pid {holder}) runs in the session");

    // What `logout` lists active now, among which this job's session is.
    let active: Vec<u64> = logout()
        .lines()
        .filter(|l| l.contains(" remote active"))
        .filter_map(|l| l.split(' ').next()?.parse().ok())
        .collect();

    // A graceful sign-out asks, and the holder, which stays, is what it waits for.
    control.send(wire::MSG_SIGN_OUT, &SignOut { force: false }.encode());
    let waiting = control.waiting();
    assert!(waiting.names.iter().any(|n| n == HOLDER), "the sign-out waits for {waiting:?}");
    let said = logout();
    let ending = said.lines().find(|l| l.contains(" remote ending ") && l.contains(HOLDER));
    let share: Option<u64> = ending.and_then(|l| l.split(' ').next()?.parse().ok());
    let share = share.unwrap_or_else(|| panic!("logout listed no session ending for the holder: {said:?}"));
    assert!(active.contains(&share), "session {share}, ending, was not listed active before: {active:?}");
    println!("session_lifecycle: logout lists session {share} active, then ending");
    println!("session_lifecycle: the sign-out waits for the holder");

    // A cancel keeps the session, and the holder in it.
    control.send(wire::MSG_CANCEL, &[]);
    control.expect(wire::MSG_CANCELLED);
    assert!(live(HOLDER).contains(&holder), "the holder ended with a cancelled sign-out");
    println!("session_lifecycle: a cancel keeps the session and the holder");

    // Only a force ends what stays; the owner ends with it.
    control.send(wire::MSG_SIGN_OUT, &SignOut { force: false }.encode());
    let _ = control.waiting();
    control.send(wire::MSG_SIGN_OUT, &SignOut { force: true }.encode());
    control.ended();
    // The roster has no readiness to wait on, so it is read until the
    // holder is gone, under the ceiling.
    wait_for(|| (!live(HOLDER).contains(&holder)).then_some(()), "the forced holder to end");
    println!("session_lifecycle: a force ends the holder, and the session ends with its owner");

    // Opens with no failure before them are never counted.
    for i in 0..OPENS {
        let (_, control) = wire::open(&login).unwrap_or_else(|why| panic!("open {i} of {OPENS}: {why}"));
        let mut control = Control { conn: control, rx: ipc::FrameRx::new() };
        control.send(wire::MSG_SIGN_OUT, &SignOut { force: false }.encode());
        control.ended();
    }
    println!("session_lifecycle: {OPENS} sessions opened and signed out in a row");

    // The machine's stop asks every session with a terminate, which the
    // holder ignores, and waits for it until a terminate forces the stop.
    let (launcher, _control) = wire::open(&login).expect("a session for the stop");
    let holder = detach(&launcher);
    let pending = power::stop(Stop::Reboot, Mode::Wait).expect("a waiting reboot is taken");
    let cancelled = pending.ticket;
    let pending = waits_for_the_holder(pending);
    power::cancel().expect("the waiting reboot cancels");
    match pending.outcome() {
        Update::Cancelled => {}
        other => panic!("a cancelled reboot ended {other:?}"),
    }
    assert!(live(HOLDER).contains(&holder), "the holder ended with a cancelled reboot");
    println!("session_lifecycle: a cancelled reboot keeps the machine and the holder");
    let pending = waits_for_the_holder(power::stop(Stop::Reboot, Mode::Wait).expect("a second waiting reboot is taken"));
    assert_ne!(pending.ticket, cancelled, "a second reboot was given the cancelled one's ticket");
    println!("session_lifecycle: ok, and the reboot is forced");
    pending.terminate().expect("the waiting reboot is forced");
    panic!("session_lifecycle: the forced reboot ended {:?}", pending.outcome());
}

/// `hangupholder` detached in `launcher`'s session, once it listens for its
/// quits: its pid. Until it listens a quit ends it, so a stop asked before
/// that would find nothing to wait for.
fn detach(launcher: &Namespace) -> u32 {
    let before = live(HOLDER);
    let mut holder = Command::new(format!("/system/bin/{HOLDER}"))
        .independent()
        .detached()
        .launcher(launcher.as_handle().into())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .current_dir("/")
        .spawn()
        .expect("detach the holder in the session");
    let mut said = String::new();
    BufReader::new(holder.stdout.take().expect("the holder's stdout"))
        .read_line(&mut said)
        .expect("the holder's first line");
    assert_eq!(said.trim_end(), "hangupholder: holding", "the holder said something else first");
    live(HOLDER).into_iter().find(|pid| !before.contains(pid)).expect("the detached holder runs")
}

/// The stop's lists until one names the holder: the first may be read before
/// its session has reported.
fn waits_for_the_holder(mut pending: Pending) -> Pending {
    let mut waiting = pending.waiting.clone();
    while !waiting.names.iter().any(|n| n == HOLDER) {
        waiting = match pending.read() {
            Update::Waiting(now) => now,
            other => panic!("a waiting reboot ended {other:?} before it named the holder"),
        };
    }
    println!("session_lifecycle: the reboot waits for the holder");
    pending
}

struct Control {
    conn: Connection,
    rx: ipc::FrameRx<{ ipc::MAX_FRAME_LEN as usize }>,
}

impl Control {
    fn send(&self, msg: u32, payload: &[u8]) {
        self.conn.send_bytes(msg, payload).expect("the session's owner takes its control's request");
    }

    /// The next frame, or `None` once the owner has hung up.
    fn next(&mut self) -> Option<(u32, Vec<u8>)> {
        let poller = Poller::new(1);
        let deadline = Instant::now() + CEILING;
        loop {
            match self.rx.pump(&self.conn) {
                RxStep::Frame { msg_type, payload_len, .. } => return Some((msg_type, self.rx.payload(payload_len).to_vec())),
                RxStep::Eof => return None,
                RxStep::Malformed => panic!("the session's owner sent a frame its control cannot describe"),
                RxStep::Idle => {}
            }
            let left = deadline.checked_duration_since(Instant::now()).expect("the session's owner said nothing past the ceiling");
            poller.watch(&self.conn, READABLE, 0);
            poller.wait(1, left.as_nanos() as u64, |_| {});
        }
    }

    fn waiting(&mut self) -> Names {
        match self.next() {
            Some((wire::MSG_WAITING, payload)) => Names::decode(&payload).expect("a report the owner writes"),
            other => panic!("the sign-out answered {other:?}, not what it waits for"),
        }
    }

    fn expect(&mut self, msg: u32) {
        loop {
            match self.next() {
                Some((m, _)) if m == msg => return,
                Some((wire::MSG_WAITING, _)) => continue,
                other => panic!("the owner answered {other:?}, not {msg}"),
            }
        }
    }

    /// Every report until the owner hangs up, which its end is.
    fn ended(&mut self) {
        while let Some((msg, _)) = self.next() {
            assert_eq!(msg, wire::MSG_WAITING, "a sign-out reported {msg}");
        }
    }
}

fn cap() -> &'static SysCap {
    static CAP: std::sync::OnceLock<SysCap> = std::sync::OnceLock::new();
    CAP.get_or_init(|| Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows this job its system capability"))
}

/// The pids of every live process named `name`.
fn live(name: &str) -> Vec<u32> {
    const HEADER: usize = toyos::system::SYSINFO_HEADER_SIZE;
    const ENTRY: usize = toyos::system::SYSINFO_ENTRY_SIZE;
    let mut buf = vec![0u8; HEADER + ENTRY * 1024];
    let n = cap().roster(&mut buf);
    assert!((HEADER..=buf.len()).contains(&n), "sysinfo answered {n}");
    (HEADER..)
        .step_by(ENTRY)
        .take_while(|pos| pos + ENTRY <= n)
        .map(|pos| &buf[pos..pos + ENTRY])
        .filter(|entry| entry[9] == 0 && entry[8] != ZOMBIE)
        .filter(|entry| String::from_utf8_lossy(&entry[32..60]).trim_end_matches('\0') == name)
        .map(|entry| u32::from_le_bytes(entry[0..4].try_into().unwrap()))
        .collect()
}

/// `check` until it answers, under the hang ceiling, which names what stalled.
fn wait_for<T>(mut check: impl FnMut() -> Option<T>, what: &str) -> T {
    let deadline = Instant::now() + CEILING;
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(Instant::now() < deadline, "session_lifecycle: stalled waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
