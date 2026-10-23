//! A declared row's view holds its own `/state/<name>` and no other part of
//! `/state` (`toyos_manifest::Program::view`), and a job inherits exactly that.
//!
//! It runs as a job of test-runner, whose row declares `state = true`: so the
//! supervisor made `/state/test-runner`, and the job can write there and
//! reaches neither the supervisor's state nor a service's. What crosses the
//! kernel is the grant set the supervisor minted, which no host test holds.

use std::fs;

const OWN: &str = "/state/test-runner";

/// Another row's state: logkeeper is a service in every image, so the
/// supervisor made it before this ran.
const SERVICE: &str = "/state/logkeeper";

fn main() {
    let names = toyos::endow::namespace().expect("a job inherits test-runner's namespace");
    let mut red = Vec::new();

    for held in ["fs:/state", "fs:/state/supervisor", "fs:/state/logkeeper"] {
        if names.open(held).is_ok() {
            red.push(format!("it holds {held}"));
        }
    }
    if let Err(e) = names.open(&format!("fs:{OWN}")) {
        red.push(format!("fs:{OWN} would not connect: {e:?}"));
    }

    let file = format!("{OWN}/state_view");
    match fs::write(&file, b"kept").and_then(|()| fs::read(&file)) {
        Ok(bytes) if bytes == b"kept" => {}
        Ok(bytes) => red.push(format!("{file} read back {bytes:?}")),
        Err(e) => red.push(format!("{file} was not written and read back: {e}")),
    }
    let _ = fs::remove_file(&file);

    // Another row's state and the supervisor's store, by name: no capability
    // names either, so each reaches only the kernel's bare mount point.
    for path in [SERVICE, "/state/supervisor/grants"] {
        if let Ok(meta) = fs::metadata(path) {
            red.push(format!("{path} answered metadata ({} bytes)", meta.len()));
        }
    }
    if fs::write(format!("{SERVICE}/planted"), b"x").is_ok() {
        red.push(format!("a file was written into {SERVICE}"));
    }
    // A grant line appended to the supervisor's store, the write a shell could
    // make while every row saw the whole tree.
    let grants = std::fs::OpenOptions::new().append(true).create(true).open("/state/supervisor/grants");
    if let Ok(mut store) = grants {
        use std::io::Write;
        if store.write_all(b"grant toy planted 00 read-write /home/toy/Documents\n").is_ok() {
            red.push("a grant line was appended to /state/supervisor/grants".to_string());
        }
    }
    // `/state` with no capability is a mount point of the kernel's, and lists
    // nothing of what the file server holds under it.
    if let Ok(listing) = fs::read_dir("/state") {
        let found: Vec<_> = listing.filter_map(Result::ok).map(|e| e.file_name()).collect();
        if !found.is_empty() {
            red.push(format!("/state lists {found:?}"));
        }
    }

    if !red.is_empty() {
        for line in &red {
            println!("  RED {line}");
        }
        std::process::exit(1);
    }
    println!("state_view: its own {OWN} and nothing else of /state");
}
