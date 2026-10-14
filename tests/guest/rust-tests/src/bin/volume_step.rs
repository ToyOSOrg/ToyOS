//! One step of the master level on soundserver's `volume` port and back,
//! within the quiet range: from the bottom one position up and down again,
//! each answer the codec's own readback.

#[path = "../master.rs"]
mod master;

fn main() {
    let volume = master::connect();
    let bottom = master::to_bottom(&volume).bottom;
    assert!(bottom > 0, "a master with one position has no step to take");
    let up = master::step(&volume, -1);
    println!("step up: position {} of {}, muted {}", up.position, up.bottom, up.muted);
    assert_eq!((up.position, up.muted), (bottom - 1, 0), "one step up reads back one position above the bottom");
    let down = master::step(&volume, 1);
    println!("step down: position {} of {}, muted {}", down.position, down.bottom, down.muted);
    assert_eq!((down.position, down.muted), (bottom, 0), "the step back reads back the bottom");
}
