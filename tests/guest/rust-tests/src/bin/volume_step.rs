//! One step of the master level on soundserver's `volume` port and back,
//! each answer the codec's own readback: the step reads back one position
//! down and unmuted, and the step back leaves the level at the top.

use toyos::audio::{Volume, VolumeStep, MSG_VOLUME, MSG_VOLUME_STEP};

fn step(volume: &toyos::Connection, delta: i32) -> Volume {
    volume.send(MSG_VOLUME_STEP, &VolumeStep { delta }).expect("the step is sent");
    let header = volume.recv_header().expect("soundserver answers the step");
    assert_eq!(header.msg_type, MSG_VOLUME, "soundserver answered the step with message {}", header.msg_type);
    volume.recv_payload(&header).expect("the answer is a volume")
}

fn main() {
    let volume = toyos::endow::service("volume").expect("this job holds soundserver's volume port");
    let down = step(&volume, 1);
    println!("step down: position {} of {}, muted {}", down.position, down.bottom, down.muted);
    assert_eq!((down.position, down.muted), (1, 0), "one step down reads back one position below the top");
    let top = step(&volume, -1);
    println!("step up: position {} of {}, muted {}", top.position, top.bottom, top.muted);
    assert_eq!((top.position, top.muted), (0, 0), "the step back reads back the top");
}
