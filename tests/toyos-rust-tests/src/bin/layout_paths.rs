//! Where a fresh boot puts things: the session user's home, each service's own
//! `/state`, the machine's keyboard layout in `/config`, the shell's history in
//! its own folder, and no dotfile anywhere ToyOS's own programs write.
//!
//! Driven by `layout_fresh_boot` alone, over ssh on `tests/layoutcase`, after
//! `locale <argv[1]>` and an interactive shell that ran `<argv[2]>`. No row
//! declares this binary, so sshd, a service whose own `HOME` is `/state/sshd`,
//! spawns it directly, and the `HOME` it reads is the one init answered for it.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

/// init's `HOME_FOLDERS`: the session home's listing, exactly.
const HOME_FOLDERS: [&str; 8] =
    ["Apps", "Desktop", "Documents", "Downloads", "Fonts", "Music", "Pictures", "Videos"];

/// The services `tests/layoutcase` runs: `/state`'s listing, exactly.
const SERVICES: [&str; 3] = ["logd", "netd", "sshd"];

/// Every volume a ToyOS program writes to.
const WRITTEN: [&str; 6] = ["/apps", "/config", "/home", "/log", "/state", "/tmp"];

/// Asked by literal path, so a constant that moved is a red here.
const LAYOUT_FILE: &str = "/config/keyboard-layout";
const HISTORY_FILE: &str = "/home/toy/Apps/shell/State/history";

/// The directory names `dir` lists, sorted.
fn listed_dirs(dir: &str) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("read_dir {dir}: {e}"))? {
        let entry = entry.map_err(|e| format!("an entry of {dir}: {e}"))?;
        if entry.file_type().map_err(|e| format!("{dir}: {e}"))?.is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, layout, typed] = args.as_slice() else {
        panic!("usage: layout_paths <layout locale set> <line the shell ran>, got {args:?}");
    };
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
    for (dir, want) in [("/home/toy", &HOME_FOLDERS[..]), ("/state", &SERVICES[..])] {
        match listed_dirs(dir) {
            Ok(names) if names == want => {}
            other => wrong.push(format!("{dir} lists {other:?}, want exactly {want:?}")),
        }
    }
    match fs::metadata("/state/sshd/host_ed25519") {
        Ok(meta) if meta.is_file() && meta.len() > 0 => {}
        other => wrong.push(format!("/state/sshd/host_ed25519: {other:?}, want a file")),
    }
    match fs::read_to_string(LAYOUT_FILE) {
        Ok(text) if text.trim() == layout => {}
        other => wrong.push(format!("{LAYOUT_FILE}: {other:?}, want {layout:?}")),
    }
    match fs::read_to_string(HISTORY_FILE) {
        Ok(text) if text.lines().any(|line| line == typed) => {}
        other => wrong.push(format!("{HISTORY_FILE}: {other:?}, want a line {typed:?}")),
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
        "layout: HOME=/home/toy, /home/toy lists {HOME_FOLDERS:?}, /state lists {SERVICES:?} \
         with sshd's key, {LAYOUT_FILE} and {HISTORY_FILE} as written, and none of {walked} \
         entries under {WRITTEN:?} is a dotfile"
    );
}
