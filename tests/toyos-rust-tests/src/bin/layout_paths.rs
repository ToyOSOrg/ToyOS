//! Where a fresh boot puts things: the session user's home, each service's own
//! `/state`, and no dotfile anywhere ToyOS's own programs write.
//!
//! Driven by `layout_fresh_boot` alone, on `tests/sshdcase` once sshd has
//! minted its identity: that boot runs three services and a session program
//! (`test-runner`, whose `HOME` this binary inherits). The dotfile walk sees
//! every file, and a directory only once it holds one.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

/// init's `HOME_FOLDERS`, spelled again: the two are two spellings of the
/// session home, so a folder added to one reds here.
const HOME_FOLDERS: [&str; 8] =
    ["Apps", "Desktop", "Documents", "Downloads", "Fonts", "Music", "Pictures", "Videos"];

/// The services `tests/sshdcase` runs, each of which init gave a `/state` of
/// its own.
const SERVICES: [&str; 3] = ["logd", "netd", "sshd"];

/// Every volume a ToyOS program writes to.
const WRITTEN: [&str; 6] = ["/apps", "/config", "/home", "/log", "/state", "/tmp"];

fn main() {
    let mut wrong = Vec::new();

    let home = std::env::var("HOME");
    if home.as_deref() != Ok("/home/toy") {
        wrong.push(format!("HOME is {home:?}, want /home/toy"));
    }
    let std_home = std::env::home_dir();
    if std_home.as_deref() != Some(Path::new("/home/toy")) {
        wrong.push(format!("std::env::home_dir() is {std_home:?}, want /home/toy"));
    }

    match fs::metadata("/home/root") {
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        other => wrong.push(format!("/home/root: {other:?}, want NotFound")),
    }
    // Asked by name and not listed: a directory on DATA is the VFS's own
    // record, and no listing of its parent shows it
    // (`issues/filesystem/a-directory-on-data-is-in-no-listing-and-no-reboot.md`).
    let dirs = ["/home/toy".to_string()]
        .into_iter()
        .chain(HOME_FOLDERS.iter().map(|f| format!("/home/toy/{f}")))
        .chain(SERVICES.iter().map(|s| format!("/state/{s}")));
    for dir in dirs {
        match fs::metadata(&dir) {
            Ok(meta) if meta.is_dir() => {}
            other => wrong.push(format!("{dir}: {other:?}, want a directory")),
        }
    }
    match fs::metadata("/state/sshd/host_ed25519") {
        Ok(meta) if meta.is_file() && meta.len() > 0 => {}
        other => wrong.push(format!("/state/sshd/host_ed25519: {other:?}, want a file")),
    }

    let mut dotted = Vec::new();
    let mut walked = 0usize;
    let mut pending: Vec<String> = WRITTEN.iter().map(|d| d.to_string()).collect();
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {dir}: {e}")) {
            let entry = entry.expect("dir entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = format!("{dir}/{name}");
            walked += 1;
            if name.starts_with('.') {
                dotted.push(path.clone());
            }
            if entry.file_type().expect("file type").is_dir() {
                pending.push(path);
            }
        }
    }
    if !dotted.is_empty() {
        wrong.push(format!("a dotfile on a fresh boot: {dotted:?}"));
    }

    assert!(wrong.is_empty(), "the layout is not as ruled:\n{}", wrong.join("\n"));
    println!(
        "layout: HOME=/home/toy, {} home folders, /state holds {SERVICES:?} with sshd's key, \
         and none of {walked} entries under {WRITTEN:?} is a dotfile",
        HOME_FOLDERS.len()
    );
}
