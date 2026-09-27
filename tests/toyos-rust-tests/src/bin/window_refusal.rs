//! A client must survive a compositor that says no.
//!
//! The window protocol had no refusal message at all: the only answer to
//! `MSG_CREATE_WINDOW` was `MSG_WINDOW_CREATED`, so a compositor that could not
//! afford another window had no move except to serve it or drop the connection,
//! and `Window::create` met anything else with `assert_eq!` — a client killed
//! by an answer it should have been able to read.
//!
//! **This binary plays the compositor, and how it does that is the point.** It
//! used to squat `services::listen("compositor")` on a boot where no compositor
//! ran, which worked because any process could take any name. There is no name
//! to take now. Instead it creates a port, builds a namespace mapping
//! `"compositor"` to that port's connector, and spawns a child holding it: the
//! child's `Window::create` reaches this process and nothing else, no other
//! process can see the service. That is the pattern every hostile-server test
//! uses from here.
//!
//! Roles: no argument is the server; `client` is the child that asks for a
//! window and decodes what comes back.

use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};

use toyos::endow::{self, EndowError};
use toyos::port::Acceptor;
use toyos::AsHandle;
use toyos::{ipc, namespace, port};
use toyos_abi::syscall::SVC_LABEL;
use window::{CreateError, Window};

const SELF_PATH: &str = "/system/bin/test_rs_window_refusal";

/// What the stand-in compositor does with one `MSG_CREATE_WINDOW`.
#[derive(Clone, Copy, Debug)]
enum Reply {
    Refuse(u32),
    /// A frame that answers nothing this request asked.
    Unrelated,
    /// No answer: the connection closes.
    Close,
    /// `MSG_WINDOW_CREATED` with no buffer sent ahead of it.
    NoBuffer,
}

/// The reply, and the `CreateError` the client must turn it into.
const CASES: &[(Reply, CreateError)] = &[
    (Reply::Refuse(window::REFUSED_AT_CAPACITY), CreateError::AtCapacity),
    (Reply::Refuse(window::REFUSED_TOO_LARGE), CreateError::TooLarge),
    // A reason from a newer compositor than this client. It must arrive as a
    // refusal carrying the raw value, not as a protocol error and not as a
    // window.
    (Reply::Refuse(4242), CreateError::Refused(4242)),
    (Reply::Unrelated, CreateError::Protocol(window::MSG_FRAME)),
    (Reply::Close, CreateError::BrokenOff),
    (Reply::NoBuffer, CreateError::NoHandle),
];

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("client") => client(),
        Some(other) => panic!("unknown role {other:?}"),
        None => server(),
    }
}

fn server() {
    let (acceptor, connector) =
        port::create().expect("window_refusal: the kernel refused a port");
    let child_ns = namespace::build()
        .add("compositor", &connector)
        .finish()
        .expect("window_refusal: the kernel refused a namespace");

    // The child is spawned holding exactly one connector, and it points here.
    let mut child = Command::new(SELF_PATH)
        .arg("client")
        .endow(SVC_LABEL, child_ns.into_raw().0)
        .stdout(Stdio::inherit())
        .spawn()
        .expect("window_refusal: spawn the client");

    for (reply, _) in CASES {
        serve_one(&acceptor, *reply);
    }

    let status = child.wait().expect("window_refusal: reap the client");
    assert_eq!(status.code(), Some(0), "the client did not survive the refusals");
    println!("{} refusal outcomes decoded, none panicked the client", CASES.len());
}

/// Answer one `MSG_CREATE_WINDOW`, then drop the connection — which is what the
/// compositor does after a refusal, and the reason the reply has to still be
/// readable once the writer is gone.
fn serve_one(acceptor: &Acceptor, reply: Reply) {
    let accepted = acceptor.accept().expect("accept a client");
    let handle = accepted.as_handle();
    let header = ipc::recv_header(handle).expect("request header");
    assert_eq!(header.msg_type, window::MSG_CREATE_WINDOW, "client sent the wrong request");
    let _req: window::CreateWindowRequest =
        ipc::recv_payload(handle, &header).expect("request payload");
    match reply {
        Reply::Refuse(reason) => {
            ipc::send(handle, window::MSG_WINDOW_REFUSED, &window::WindowRefused { reason })
                .expect("send the refusal");
        }
        Reply::Unrelated => {
            ipc::signal(handle, window::MSG_FRAME).expect("send a reply that answers nothing");
        }
        Reply::Close => {}
        Reply::NoBuffer => {
            let info = window::WindowInfo { width: 100, height: 100, stride: 100, pixel_format: 0 };
            ipc::send(handle, window::MSG_WINDOW_CREATED, &info)
                .expect("send a window with no buffer");
        }
    }
}

fn client() {
    for (reply, expected) in CASES {
        let outcome = Window::create(100, 100);
        let got = match outcome {
            Ok(_) => panic!("reply {reply:?} produced a window"),
            Err(e) => e,
        };
        assert_eq!(got, *expected, "reply {reply:?} decoded wrongly");
        // The message has to survive being read after the sender let go: the
        // compositor drops the connection the moment it has answered.
        assert!(!got.to_string().is_empty(), "{got:?} has no message");
    }

    // A port whose queue is full, which the stand-in never drains: the kernel
    // refuses the connection itself.
    let mut queued = Vec::new();
    let refused = loop {
        match endow::service("compositor") {
            Ok(conn) => queued.push(conn),
            Err(EndowError::Refused(e)) => break e,
            Err(other) => panic!("filling the queue ended in {other:?}"),
        }
    };
    let got = Window::create(100, 100).err();
    assert_eq!(got, Some(CreateError::Kernel(refused)), "a full queue decoded wrongly");
}
