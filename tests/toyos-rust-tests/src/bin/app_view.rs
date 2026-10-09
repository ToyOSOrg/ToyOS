//! An installed app sees its own package read-only and its own folder as
//! `HOME`, and nothing else of `/apps` or `/home`.
//!
//! The job installs this binary as the package `appview` beside another
//! package and another app's folder, and launches it through test-runner's
//! launcher, whose row lists `/apps`. Two launches are refused first: the
//! same binary installed as `shell`, a row the image declares, whose folder
//! of the home is the shell's; and `appview` while a file stands where its
//! folder goes. Run as the app (`app`), it asks:
//!
//! - its `HOME` is `/home/toy/Apps/appview`, holding `Config Data Cache State`,
//!   and a file it writes there lands in that folder of `/home`;
//! - its own package reads, and every request that would change it — an open
//!   to write, append, truncate, create or create anew, a `mkdir`, `rmdir`,
//!   `unlink`, `rename` and `symlink` — is refused `PermissionDenied` by the
//!   server, past std, and std's own write is too;
//! - it holds no other directory: not `/apps`, another package, `/home`, the
//!   session's home, another app's folder, `/config`, `/state`, `/log` or
//!   `/boot`, and none of their files is there by path.
//!
//! Every arm runs, so one run names each one that is red. The job then holds
//! the package to what it installed.

use std::fs;
use std::io::ErrorKind;
use std::process::Command;

use toyos::fs::{Dir, Refused, O_APPEND, O_CREATE, O_CREATE_NEW, O_READ, O_TRUNCATE, O_WRITE};
use toyos_abi::syscall::SyscallError;

const SELF: &str = "/system/bin/test_rs_app_view";
const PACKAGE: &str = "/apps/appview";
const PROGRAM: &str = "/apps/appview/appview";
/// The layout's `/home/<user>/Apps/<name>`, spelled here and not asked of the
/// manifest crate the supervisor reads.
const HOME: &str = "/home/toy/Apps/appview";
const MANIFEST: &[u8] = b"name = \"appview\"\nversion = \"1\"\n\
    digest = \"0000000000000000000000000000000000000000000000000000000000000000\"\n\
    program = \"/apps/appview/appview\"\n";
/// A package named after the shell's row, whose `Apps/shell` is the shell's.
const ROW_PACKAGE: &str = "/apps/shell";
const ROW_PROGRAM: &str = "/apps/shell/shell";
const ROW_MANIFEST: &[u8] = b"name = \"shell\"\nversion = \"1\"\n\
    digest = \"0000000000000000000000000000000000000000000000000000000000000000\"\n\
    program = \"/apps/shell/shell\"\n";
const OTHER_PACKAGE_FILE: &str = "/apps/other/kept";
const OTHER_FOLDER_FILE: &str = "/home/toy/Apps/other/Data/kept";
const KEPT: &[u8] = b"what the app wrote in its own folder";
const APP: &str = "app";

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some(APP) => app(),
        _ => job(),
    }
}

fn job() {
    for dir in [PACKAGE, ROW_PACKAGE] {
        let _ = fs::remove_dir_all(dir);
    }
    let _ = fs::remove_dir_all(HOME);
    let _ = fs::remove_file(HOME);
    fs::create_dir_all(format!("{PACKAGE}/sub")).expect("make the package's directories");
    fs::copy(SELF, PROGRAM).expect("install this binary as the package's program");
    fs::write(format!("{PACKAGE}/manifest.toml"), MANIFEST).expect("write the package's manifest");
    fs::create_dir_all(ROW_PACKAGE).expect("make the row-named package's directory");
    fs::copy(SELF, ROW_PROGRAM).expect("install this binary as the row-named package's program");
    fs::write(format!("{ROW_PACKAGE}/manifest.toml"), ROW_MANIFEST).expect("write the row-named package's manifest");
    for file in [OTHER_PACKAGE_FILE, OTHER_FOLDER_FILE] {
        let dir = file.rsplit_once('/').expect("a file in a directory").0;
        fs::create_dir_all(dir).unwrap_or_else(|e| panic!("make {dir}: {e}"));
        fs::write(file, b"not the app's").unwrap_or_else(|e| panic!("write {file}: {e}"));
    }
    let mut red = Vec::new();

    // Refused, and the supervisor says why (the metal row's judge reads it).
    match Command::new(ROW_PROGRAM).arg(APP).output() {
        Err(e) => println!("  a package named after the shell's row: refused ({e})"),
        Ok(ran) => red.push(format!("a package named after the shell's row ran: {ran:?}")),
    }
    // A launch that went ahead above may have left a folder there.
    match fs::write(HOME, b"no folder") {
        Err(e) => red.push(format!("no file could be planted where the app's folder goes: {e}")),
        Ok(()) => {
            match Command::new(PROGRAM).arg(APP).output() {
                Err(e) => println!("  a package whose folder is a file: refused ({e})"),
                Ok(ran) => red.push(format!("a package whose folder is a file ran: {ran:?}")),
            }
            fs::remove_file(HOME).expect("take the planted file away");
        }
    }

    let ran = Command::new(PROGRAM).arg(APP).output().expect("launch the package through the launcher");
    print!("{}", String::from_utf8_lossy(&ran.stdout));
    print!("{}", String::from_utf8_lossy(&ran.stderr));
    if ran.status.code() != Some(0) {
        red.push(format!("the app ended {:?}", ran.status));
    }
    match fs::read(format!("{PACKAGE}/manifest.toml")) {
        Ok(bytes) if bytes == MANIFEST => {}
        other => red.push(format!("the package's manifest is not what was installed: {other:?}")),
    }
    let mut listed: Vec<String> = fs::read_dir(PACKAGE)
        .expect("list the package")
        .map(|e| e.expect("an entry").file_name().to_string_lossy().into_owned())
        .collect();
    listed.sort();
    if listed != ["appview", "manifest.toml", "sub"] {
        red.push(format!("the package holds {listed:?}, not what was installed"));
    }
    match fs::read(format!("{HOME}/Data/kept")) {
        Ok(bytes) if bytes == KEPT => println!("  the app's write is in {HOME}/Data"),
        other => red.push(format!("what the app wrote is not in {HOME}/Data/kept: {other:?}")),
    }
    for dir in [PACKAGE, ROW_PACKAGE, "/apps/other", "/home/toy/Apps/other", HOME] {
        let _ = fs::remove_dir_all(dir);
    }
    if !red.is_empty() {
        panic!("app_view: {red:#?}");
    }
    println!("app_view: the app saw its own package read-only and its own folder as HOME, and nothing else");
}

/// Every arm, as the package `appview`: `red` names each one that failed.
fn app() {
    let mut red: Vec<String> = Vec::new();

    let home = std::env::home_dir();
    if home.as_deref() != Some(std::path::Path::new(HOME)) {
        red.push(format!("home_dir() is {home:?}, not {HOME}"));
    }
    for folder in ["Config", "Data", "Cache", "State"] {
        if !fs::metadata(format!("{HOME}/{folder}")).is_ok_and(|m| m.is_dir()) {
            red.push(format!("{HOME}/{folder} is not a directory"));
        }
    }
    if let Err(e) = fs::write(format!("{HOME}/Data/kept"), KEPT) {
        red.push(format!("a write in its own folder was refused: {e}"));
    }

    match fs::read(format!("{PACKAGE}/manifest.toml")) {
        Ok(bytes) if bytes == MANIFEST => println!("  its own package reads"),
        other => red.push(format!("its own manifest did not read back: {other:?}")),
    }
    match fs::write(format!("{PACKAGE}/made"), b"x") {
        Err(e) if e.kind() == ErrorKind::PermissionDenied => println!("  std's write into its package: refused"),
        other => red.push(format!("std's write into its own package was answered {other:?}")),
    }

    let names = toyos::endow::namespace().expect("an app is endowed a namespace");
    match Dir::connect(names, &format!("fs:{PACKAGE}")) {
        Err(e) => red.push(format!("fs:{PACKAGE} would not connect: {e:?}")),
        Ok(mut dir) => {
            if dir.writable() {
                red.push(format!("fs:{PACKAGE} says it is writable"));
            }
            match dir.open("manifest.toml", O_READ) {
                Ok(opened) => dir.close(opened.fid, opened.generation),
                Err(e) => red.push(format!("an open to read its manifest was refused: {e:?}")),
            }
            let denied = Refused::Error(SyscallError::PermissionDenied);
            let mut each = |what: &str, answer: Result<(), Refused>| match answer {
                Err(e) if e == denied => println!("  {what}: refused"),
                other => red.push(format!("{what} in its own package was answered {other:?}")),
            };
            for (flags, what) in [
                (O_WRITE, "an open to write"),
                (O_READ | O_APPEND, "an open to append"),
                (O_READ | O_TRUNCATE, "an open to truncate"),
            ] {
                each(what, dir.open("manifest.toml", flags).map(|o| dir.close(o.fid, o.generation)));
            }
            each("an open to create", dir.open("made", O_WRITE | O_CREATE).map(|o| dir.close(o.fid, o.generation)));
            each(
                "an open to create anew",
                dir.open("made", O_WRITE | O_CREATE_NEW).map(|o| dir.close(o.fid, o.generation)),
            );
            each("mkdir", dir.mkdir("made"));
            each("rmdir", dir.rmdir("sub"));
            each("unlink", dir.unlink("manifest.toml"));
            each("rename", dir.rename("manifest.toml", "moved"));
            each("symlink", dir.symlink("manifest.toml", "link"));
        }
    }
    match Dir::connect(names, &format!("fs:{HOME}")) {
        Ok(dir) if dir.writable() => {}
        Ok(_) => red.push(format!("fs:{HOME} says it is read-only")),
        Err(e) => red.push(format!("fs:{HOME} would not connect: {e:?}")),
    }

    for held in [
        "fs:/apps",
        "fs:/apps/other",
        "fs:/home",
        "fs:/home/toy",
        "fs:/home/toy/Apps",
        "fs:/home/toy/Apps/other",
        "fs:/config",
        "fs:/state",
        "fs:/log",
        "fs:/boot",
    ] {
        match names.open(held) {
            Err(_) => {}
            Ok(_) => red.push(format!("it holds {held}")),
        }
    }
    for file in [OTHER_PACKAGE_FILE, OTHER_FOLDER_FILE] {
        match fs::read(file) {
            Err(_) => {}
            Ok(bytes) => red.push(format!("{file} read {} bytes", bytes.len())),
        }
    }
    // A directory no capability names is a mount point of the kernel's, and
    // lists nothing of what the file server holds under it.
    for dir in ["/apps", "/home", "/home/toy", "/home/toy/Apps", "/config", "/state", "/log"] {
        if let Ok(listing) = fs::read_dir(dir) {
            let names: Vec<_> = listing.filter_map(Result::ok).map(|e| e.file_name()).collect();
            if !names.is_empty() {
                red.push(format!("{dir} lists {names:?}"));
            }
        }
    }

    if !red.is_empty() {
        for line in &red {
            println!("  RED {line}");
        }
        std::process::exit(1);
    }
    println!("  every arm held");
}
