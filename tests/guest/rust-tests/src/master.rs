//! soundserver's master on its `volume` port, each answer the codec's own
//! readback.

use toyos::audio::{Volume, VolumeStep, MSG_VOLUME, MSG_VOLUME_STEP};

pub fn connect() -> toyos::Connection {
    toyos::endow::service("volume").expect("this job holds soundserver's volume port")
}

pub fn step(volume: &toyos::Connection, delta: i32) -> Volume {
    volume.send(MSG_VOLUME_STEP, &VolumeStep { delta }).expect("the step is sent");
    let header = volume.recv_header().expect("soundserver answers the step");
    assert_eq!(header.msg_type, MSG_VOLUME, "soundserver answered the step with message {}", header.msg_type);
    volume.recv_payload(&header).expect("the answer is a volume")
}

/// Step the master to its bottom, the lowest position that is not muted.
pub fn to_bottom(volume: &toyos::Connection) -> Volume {
    let bottom = step(volume, i32::MAX);
    println!("master at the bottom: position {} of {}, muted {}", bottom.position, bottom.bottom, bottom.muted);
    assert_eq!((bottom.position, bottom.muted), (bottom.bottom, 0), "the master reads back at its bottom, unmuted");
    bottom
}
