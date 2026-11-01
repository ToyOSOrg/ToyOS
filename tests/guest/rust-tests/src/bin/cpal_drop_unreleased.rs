//! cpal's ToyOS host, dropped while its stream's server never lets go of it.
//!
//! This binary is the stream's server: it describes its device and answers the
//! open as soundserver does, and
//! then holds the stream's signal without raising it until the client's
//! `Drop` has come back — a soundserver whose mix loop has stopped. Its child, the
//! same binary with `client`, reaches it as `soundserver`, builds a stream through
//! cpal and drops it unplayed. The stream thread sends the close and waits on
//! the signal; `Drop` has to come back with the refusal by name through the error
//! callback rather than wait for good. One signal then has to end the stream
//! thread while its process lives, so the stream's connection closes as it
//! would for soundserver.
//!
//! No sound is played and soundserver is not involved.

use std::io::{BufRead, BufReader, Read};
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;

use cpal::traits::{DeviceTrait, HostTrait};
use toyos::audio::{
    DeviceDescription, StreamOpenRequest, StreamOpenResponse, MSG_DEVICE_DESCRIBE,
    MSG_DEVICE_DESCRIBED, MSG_STREAM_CLOSE, MSG_STREAM_OPEN, MSG_STREAM_OPENED,
};
use toyos::ipc::IpcError;
use toyos::shm::SharedMemory;
use toyos::{namespace, port};
use toyos_abi::audio::AudioSlotHeader;
use toyos_abi::syscall::SVC_LABEL;

const SELF: &str = "/system/bin/test_rs_cpal_drop_unreleased";

/// cpal's ToyOS host's words for a soundserver that did not let go.
const REFUSAL: &str = "soundserver did not let go of the closed stream within ";

/// The device this server describes: soundserver's on the T14.
const DEVICE: DeviceDescription = DeviceDescription {
    sample_rate: 44_100,
    period_frames: 128,
    min_client_rate: 8_000,
    max_client_rate: 192_000,
    channels: 2,
    _pad: 0,
};

/// What soundserver answers an open at the device's own config with, which is
/// the one stream the client opens.
const OPENED: StreamOpenResponse = StreamOpenResponse {
    client_period_frames: 128,
    client_period_bytes: 512,
    device_sample_rate: 44_100,
    device_channels: 2,
    slot_count: 8,
    fade_frames: 220,
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
        .add("soundserver", &connector)
        .finish()
        .expect("a namespace naming this binary soundserver");
    let mut child = Command::new(SELF)
        .arg("client")
        .endow(SVC_LABEL, names.into())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the client");

    // cpal asks for the device on a connection of its own each time it needs
    // it, and the stream is the connection whose first frame is an open.
    let conn = loop {
        let conn = acceptor.accept().expect("the client's connection");
        let mut rx = toyos::ipc::FrameRx::<{ toyos::ipc::MAX_FRAME_LEN as usize }>::new();
        let frame = rx.recv(&conn).expect("the client's first frame");
        match frame.msg_type {
            MSG_DEVICE_DESCRIBE => conn.send(MSG_DEVICE_DESCRIBED, &DEVICE).expect("the description"),
            MSG_STREAM_OPEN => {
                let _: StreamOpenRequest = toyos::ipc::decode_payload(rx.payload(frame.payload_len)).expect("the client's open");
                break conn;
            }
            other => panic!("the client's first frame is message {other}, neither a describe nor an open"),
        }
    };
    let ring = SharedMemory::create(
        AudioSlotHeader::SIZE + OPENED.slot_count as usize * OPENED.client_period_bytes as usize,
    )
    .expect("a ring");
    let (signal, signal_bell) = toyos::wake::pair().expect("a signal event");
    conn.send_with(MSG_STREAM_OPENED, &OPENED, [ring.share().expect("the ring to share"), signal_bell.into()])
        .expect("the open answered with its handles");

    let mut rx = toyos::ipc::FrameRx::<{ toyos::ipc::MAX_FRAME_LEN as usize }>::new();
    let close = rx.recv(&conn).expect("the client's close");
    assert_eq!(close.msg_type, MSG_STREAM_CLOSE, "the dropped stream did not close");

    // The signal side is held until the client has gone, so nothing but its
    // own deadline can end its wait. The client says the refusal once `Drop`
    // has returned with it.
    let mut said = String::new();
    BufReader::new(child.stdout.take().expect("the client's stdout"))
        .read_line(&mut said)
        .expect("the client's line");
    assert!(said.starts_with("client: "), "the client said {said:?} before its drop came back");
    print!("{said}");

    signal.ring().expect("a signal to the stream thread");
    match rx.recv(&conn) {
        Err(IpcError::Disconnected) => {}
        other => panic!(
            "the stream's connection gave {:?} after its close, not its end",
            other.map(|f| f.msg_type)
        ),
    }
    // The client exits once its stdin closes, so the connection's end above
    // was the stream thread's.
    drop(child.stdin.take());
    let status = child.wait().expect("wait for the client");
    drop(signal);
    assert!(status.success(), "the client exited {status:?}");
    println!(
        "cpal's drop came back while its server held the signal, and the next signal \
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
