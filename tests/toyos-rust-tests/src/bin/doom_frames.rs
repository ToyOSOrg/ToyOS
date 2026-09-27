//! Runs doom's frame check and reports whether the process lived.
//!
//! The hashing is `frames` in `userland/doom`: it plays `demo1` as a timedemo
//! and folds each tic's frame into one number, which doom prints itself. This
//! side starts it and answers whether it exited or died; the verdict on the
//! number is the host's.

use std::process::Command;

fn main() {
    let status = Command::new("/system/bin/doom")
        .arg("--frame-check")
        .status()
        .expect("spawn /system/bin/doom --frame-check");
    assert!(status.success(), "doom's frame check did not finish: {status:?}");
    println!("doom drew its frames");
}
