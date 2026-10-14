//! One step of the master level on soundserver's `volume` port and back,
//! from the bottom one position up and down again, each answer the codec's
//! own readback; nothing plays, and the master is left muted.

#[path = "../master.rs"]
mod master;

fn main() {
    let volume = master::connect();
    let bottom = master::silence(&volume).bottom;
    assert!(bottom > 0, "a master with one position has no step to take");
    let up = master::step(&volume, -1);
    assert_eq!((up.position, up.muted), (bottom - 1, 0), "one step up reads back one position above the bottom");
    let down = master::step(&volume, 1);
    assert_eq!((down.position, down.muted), (bottom, 0), "the step back reads back the bottom");
    master::silence(&volume);
}
