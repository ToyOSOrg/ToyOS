//! `/system/bin/session`: one login session's owner, started by the supervisor.
//!
//! It holds the session's launcher acceptor, the acceptor its control
//! connections arrive on, and its link to the supervisor. Each connection on
//! the launcher goes to the supervisor with its badge, which names the caller's
//! row; the supervisor serves the launch, and tells this owner of each app it
//! starts and of each one's end. A sign-out is [`session::Owner`]'s to decide.

use toyos::endow::Endowments;
use toyos::ipc::{self, Connection, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::port::Acceptor;
use toyos::process::Process;
use toyos::{say, AsHandle};
use toyos_abi::syscall::MAX_BADGE;
use toyos_session::{self as wire, Names, SignOut, Started};

use session::{Act, Owner};

const TOKEN_LINK: u64 = 0;
const TOKEN_LAUNCHER: u64 = 1;
const TOKEN_CONTROLS: u64 = 2;
const TOKEN_CONTROL_BASE: u64 = 3;

/// The live control connections one session keeps: its login authority's,
/// and a restarted compositor's while its last one's hang-up is unread. One
/// past it is closed unread, which its holder reads as the session's end.
const MAX_CONTROLS: usize = 8;

type Rx = ipc::FrameRx<{ ipc::MAX_FRAME_LEN as usize }>;

struct Control {
    conn: Connection,
    rx: Rx,
}

fn main() {
    let endowed = Endowments::get();
    // Connected here, so the link is this job's and not the supervisor's.
    let link: Connection = endowed
        .take::<toyos::namespace::Namespace>(wire::LINK_LABEL)
        .expect("session: started without its link to the supervisor")
        .open(wire::LINK)
        .expect("session: the supervisor's link refused it");
    let launcher: Acceptor =
        endowed.take(wire::LAUNCHER_ACCEPTOR_LABEL).expect("session: started without its launcher's acceptor");
    let controls_acceptor: Acceptor =
        endowed.take(wire::CONTROL_ACCEPTOR_LABEL).expect("session: started without its controls' acceptor");

    // The place the session's detached apps are spawned under, before any
    // launch is handed over on the same link.
    let place = toyos::endow::this_process().as_handle().try_clone_to_owned().expect("session: its own handle duplicates");
    link.signal_with(wire::MSG_PLACE, [place]).expect("session: its place reaches the supervisor");

    let mut owner = Owner::default();
    let mut apps: Vec<(u64, Process)> = Vec::new();
    let mut controls: Vec<Control> = Vec::new();
    let mut link_rx = Rx::new();
    let poller = Poller::new((3 + MAX_CONTROLS) as u32);
    let mut ready = Vec::new();
    loop {
        poller.watch(&link, READABLE, TOKEN_LINK);
        poller.watch(&launcher, READABLE, TOKEN_LAUNCHER);
        poller.watch(&controls_acceptor, READABLE, TOKEN_CONTROLS);
        for (i, control) in controls.iter().enumerate() {
            poller.watch(&control.conn, READABLE, TOKEN_CONTROL_BASE + i as u64);
        }
        ready.clear();
        poller.wait(1, u64::MAX, |token| ready.push(token));

        let mut acts = Vec::new();
        if ready.contains(&TOKEN_LAUNCHER) {
            hand_over(&launcher, &link);
        }
        if ready.contains(&TOKEN_CONTROLS) {
            let conn = controls_acceptor.accept().expect("session: its own acceptor refused");
            match controls.len() < MAX_CONTROLS {
                true => controls.push(Control { conn, rx: Rx::new() }),
                false => say!("session: a control connection past {MAX_CONTROLS} is refused"),
            }
        }
        if ready.contains(&TOKEN_LINK) {
            loop {
                match link_rx.pump(&link) {
                    RxStep::Idle => break,
                    RxStep::Eof => {
                        say!("session: the supervisor's link ended");
                        std::process::exit(1);
                    }
                    RxStep::Malformed => panic!("session: the supervisor sent a frame the link cannot describe"),
                    RxStep::Frame { msg_type, payload_len, .. } => {
                        let payload = link_rx.payload(payload_len).to_vec();
                        let batch = link_rx.take_batch();
                        acts.extend(from_supervisor(&mut owner, &mut apps, msg_type, &payload, batch));
                    }
                }
            }
        }
        let mut i = 0;
        while i < controls.len() {
            if !ready.contains(&(TOKEN_CONTROL_BASE + i as u64)) {
                i += 1;
                continue;
            }
            let control = &mut controls[i];
            match control.rx.pump(&control.conn) {
                RxStep::Idle => i += 1,
                // A control that hangs up asks nothing: a compositor that ends
                // leaves its session as it was.
                RxStep::Eof | RxStep::Malformed => {
                    controls.remove(i);
                }
                RxStep::Frame { msg_type, payload_len, .. } => {
                    let payload = control.rx.payload(payload_len).to_vec();
                    match (msg_type, SignOut::decode(&payload)) {
                        (wire::MSG_SIGN_OUT, Some(asked)) => acts.extend(owner.sign_out(asked.force)),
                        (wire::MSG_CANCEL, _) => acts.extend(owner.cancel()),
                        _ => say!("session: a control asked {msg_type}, which is no request"),
                    }
                    i += 1;
                }
            }
        }
        for act in acts {
            act_on(act, &mut apps, &controls, &link);
        }
    }
}

/// One connection off the launcher, to the supervisor with its badge.
fn hand_over(launcher: &Acceptor, link: &Connection) {
    let conn = launcher.accept().expect("session: its own launcher acceptor refused");
    let mut badge = [0u8; MAX_BADGE];
    let badge = match launcher.badge(&conn, &mut badge) {
        Ok(badge) => badge.to_vec(),
        Err(e) => return say!("session: a launch through an unbadged connector is dropped ({e:?})"),
    };
    // The supervisor drains its link before it sleeps, so a full link is its
    // own stall: the caller is answered by its connection closing.
    if let Err(e) = link.try_send_bytes_with(wire::MSG_HANDOVER, &badge, [conn.into_owned()]) {
        say!("session: a launch was not handed to the supervisor: {e:?}");
    }
}

fn from_supervisor(
    owner: &mut Owner,
    apps: &mut Vec<(u64, Process)>,
    msg_type: u32,
    payload: &[u8],
    batch: ipc::Batch,
) -> Vec<Act> {
    match msg_type {
        wire::MSG_STARTED => {
            let started = Started::decode(payload).expect("session: the supervisor's start is one it writes");
            let [handle] = batch.exact::<1>().expect("session: a start carries its app");
            let process: Process = toyos::endow::FromHandle::from_handle(handle);
            apps.push((started.instance, process));
            owner.started(started.instance, started.name)
        }
        wire::MSG_ENDED => {
            let instance = wire::decode_instance(payload).expect("session: the supervisor's end is one it writes");
            apps.retain(|(i, _)| *i != instance);
            owner.ended(instance)
        }
        wire::MSG_SIGN_OUT => {
            let asked = SignOut::decode(payload).expect("session: the supervisor's sign-out is one it writes");
            owner.sign_out(asked.force)
        }
        wire::MSG_STOPPING => owner.stopping(),
        wire::MSG_CANCEL => owner.stop_cancelled(),
        other => panic!("session: the supervisor sent {other}, which the link has no word for"),
    }
}

fn act_on(act: Act, apps: &mut [(u64, Process)], controls: &[Control], link: &Connection) {
    let app = |instance| apps.iter().find(|(i, _)| *i == instance).map(|(_, p)| p);
    match act {
        Act::Quit(instance, reason) => {
            if let Some(process) = app(instance) {
                if let Err(e) = process.quit(reason) {
                    say!("session: app {instance} was not asked to quit: {e:?}");
                }
            }
        }
        Act::Kill(instance) => {
            if let Some(process) = app(instance) {
                if let Err(e) = process.kill() {
                    say!("session: app {instance} was not killed: {e:?}");
                }
            }
        }
        Act::Report(names) => report(&names, controls, link),
        Act::Stopping(stopping) => {
            match stopping {
                true => say!("session: the machine stops; every app is asked to quit"),
                false => say!("session: the machine's stop is cancelled; the sign-out asked before it goes on"),
            }
            for control in controls {
                let _ = control.conn.try_send_bytes(wire::MSG_STOPPING, &[u8::from(stopping)]);
            }
        }
        Act::Cancelled => {
            say!("session: the sign-out is cancelled");
            for control in controls {
                let _ = control.conn.try_signal(wire::MSG_CANCELLED);
            }
            let _ = link.try_signal(wire::MSG_CANCELLED);
        }
        Act::Exit => {
            say!("session: every app has ended; the session ends");
            std::process::exit(0);
        }
    }
}

/// What the sign-out waits for, to every control and to the supervisor. A
/// reader that does not drain loses this report and reads the next.
fn report(names: &Names, controls: &[Control], link: &Connection) {
    say!("session: waiting for: {}", names.names.join(", "));
    let bytes = names.encode();
    for control in controls {
        let _ = control.conn.try_send_bytes(wire::MSG_WAITING, &bytes);
    }
    let _ = link.try_send_bytes(wire::MSG_WAITING, &bytes);
}
