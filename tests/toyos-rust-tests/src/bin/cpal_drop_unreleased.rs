//! cpal's ToyOS host, dropped while its stream's server never lets go of it.
//!
//! This binary is the stream's server: it answers the open as soundd does, and
//! then holds the signal pipe open without writing it until the client's
//! `Drop` has come back — a soundd whose mix loop has stopped. Its child, the
//! same binary with `client`, reaches it as `soundd`, builds a stream through
//! cpal and drops it unplayed. The stream thread sends the close and waits on
//! the pipe; `Drop` has to come back with the refusal by name through the error
//! callback rather than wait for good. One signal then has to end the stream
//! thread while its process lives, so the stream's connection closes as it
//! would for soundd.
//!
//! No sound is played and soundd is not involved.

use std::io::{BufRead, BufReader, Read};
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;

use cpal::traits::{DeviceTrait, HostTrait};
use toyos::audio::{
    StreamOpenRequest, StreamOpenResponse, MSG_STREAM_CLOSE, MSG_STREAM_OPEN, MSG_STREAM_OPENED,
};
use toyos::ipc::IpcError;
use toyos::shm::SharedMemory;
use toyos::{namespace, port};
use toyos_abi::audio::AudioSlotHeader;
use toyos_abi::syscall::SVC_LABEL;

const SELF: &str = "/system/bin/test_rs_cpal_drop_unreleased";

/// cpal's ToyOS host's words for a soundd that did not let go.
const REFUSAL: &str = "soundd did not let go of the closed stream within ";

/// What soundd answers a 44100 Hz stereo open with, which is the one stream
/// cpal's ToyOS host opens.
const OPENED: StreamOpenResponse = StreamOpenResponse {
    client_period_frames: 128,
    client_period_bytes: 512,
    device_sample_rate: 44_100,
    device_channels: 2,
    slot_count: 8,
};

fn main() {
    match std::env::args().nth(1).as_deref() {
        None => serve(),
        Some("client") => client(),
        Some(other) => panic!("no role {other:?}"),
    }
}

fn serve() {
    let (acceptor, connector) = port::create().expect("a port");
    let names = namespace::build()
        .add("soundd", &connector)
        .finish()
        .expect("a namespace naming this binary soundd");
    let mut child = Command::new(SELF)
        .arg("client")
        .endow(SVC_LABEL, names.into_raw().0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the client");

    let conn = acceptor.accept().expect("the client's connection");
    let (kind, _): (u32, StreamOpenRequest) = conn.recv().expect("the client's open");
    assert_eq!(kind, MSG_STREAM_OPEN, "the client's first frame is not an open");
    let ring = SharedMemory::create(
        AudioSlotHeader::SIZE + OPENED.slot_count as usize * OPENED.client_period_bytes as usize,
    )
    .expect("a ring");
    let (signal_read, signal_write) = toyos::pipe_pair().expect("a signal pipe");
    conn.send_with_handles(
        &[ring.share().expect("the ring to share"), signal_read.into_raw()],
        MSG_STREAM_OPENED,
        &OPENED,
    )
    .expect("the open answered");

    let close = conn.recv_header().expect("the client's close");
    assert_eq!(close.msg_type, MSG_STREAM_CLOSE, "the dropped stream did not close");

    // The write end is held until the client has gone, so nothing but its
    // own deadline can end its wait. The client says the refusal once `Drop`
    // has returned with it.
    let mut said = String::new();
    BufReader::new(child.stdout.take().expect("the client's stdout"))
        .read_line(&mut said)
        .expect("the client's line");
    assert!(said.starts_with("client: "), "the client said {said:?} before its drop came back");
    print!("{said}");

    signal_write.write(&[1]).expect("a signal to the stream thread");
    match conn.recv_header() {
        Err(IpcError::Disconnected) => {}
        other => panic!(
            "the stream's connection gave {:?} after its close, not its end",
            other.map(|h| h.msg_type)
        ),
    }
    // The client exits once its stdin closes, so the connection's end above
    // was the stream thread's.
    drop(child.stdin.take());
    let status = child.wait().expect("wait for the client");
    drop(signal_write);
    assert!(status.success(), "the client exited {status:?}");
    println!(
        "cpal's drop came back while its server held the signal pipe, and the next signal \
         ended its stream thread"
    );
}

fn client() {
    let device = cpal::default_host()
        .default_output_device()
        .expect("the ToyOS host's output");
    let config = device.default_output_config().expect("its config");
    let (said, heard) = mpsc::channel();
    let stream = device
        .build_output_stream(
            config.into(),
            |_: &mut [i16], _: &cpal::OutputCallbackInfo| {
                unreachable!("a stream never played is never asked for audio")
            },
            move |error: cpal::Error| said.send(error).expect("main hears every error"),
            None,
        )
        .expect("the open answered");
    drop(stream);

    let errors: Vec<cpal::Error> = heard.try_iter().collect();
    assert!(
        matches!(
            errors.as_slice(),
            [refused] if refused.kind() == cpal::ErrorKind::HostUnavailable
                && refused.to_string().starts_with(REFUSAL)
        ),
        "the drop came back with {errors:?}, not the refusal"
    );
    println!("client: {}", errors[0]);

    let mut rest = Vec::new();
    std::io::stdin().read_to_end(&mut rest).expect("the server's end of stdin");
}
