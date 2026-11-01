//! soundserver's master on its `volume` port, each answer the codec's own
//! readback.

use toyos::audio::{Volume, VolumeStep, MSG_VOLUME, MSG_VOLUME_MUTE, MSG_VOLUME_STEP, MSG_VOLUME_UNAVAILABLE};

pub fn connect() -> toyos::Connection {
    toyos::endow::service("volume").expect("this job holds soundserver's volume port")
}

fn answer(volume: &toyos::Connection) -> Volume {
    let mut rx = toyos::ipc::FrameRx::<{ core::mem::size_of::<Volume>() }>::new();
    let frame = rx.recv(volume).expect("soundserver answers the request");
    assert_ne!(
        frame.msg_type, MSG_VOLUME_UNAVAILABLE,
        "soundserver has no master to set: it plays to no audio device with one"
    );
    assert_eq!(frame.msg_type, MSG_VOLUME, "soundserver answered with message {}", frame.msg_type);
    let read: Volume = toyos::ipc::decode_payload(rx.payload(frame.payload_len)).expect("the answer is a volume");
    println!("master: position {} of {}, muted {}", read.position, read.bottom, read.muted);
    read
}

/// Move the master by `delta` positions; a step unmutes it.
pub fn step(volume: &toyos::Connection, delta: i32) -> Volume {
    volume.send(MSG_VOLUME_STEP, &VolumeStep { delta }).expect("the step is sent");
    answer(volume)
}

/// Mute the master at its bottom, whatever it was: the step there unmutes,
/// so the toggle after it mutes.
pub fn silence(volume: &toyos::Connection) -> Volume {
    let bottom = step(volume, i32::MAX);
    assert_eq!((bottom.position, bottom.muted), (bottom.bottom, 0), "the master reads back at its bottom, unmuted");
    volume.send_bytes(MSG_VOLUME_MUTE, &[]).expect("the mute is sent");
    let muted = answer(volume);
    assert_eq!((muted.position, muted.muted), (bottom.bottom, 1), "the master reads back muted at its bottom");
    muted
}
