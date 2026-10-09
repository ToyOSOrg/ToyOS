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
//!
//! Then it grants the package a folder of the home, `/home/toy/Games` holding
//! a ROM, through `/system/bin/grants`: refused in this job's own session,
//! which is the machine's, and refused `Apps/appview` by name, from the login
//! session a shell opens. Granted it there, the package run as `game` browses
//! its working directory with gbae's own `list_directory` and saves beside
//! the ROM as gbae does: it starts in the folder, lists the ROM and writes
//! `test.sav`. The same package with one byte more is another binary and
//! holds no folder; restored, it holds it again; revoked, its next launch
//! lists no ROM and writes nothing.

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
const GAME: &str = "game";
const GAMES: &str = "/home/toy/Games";
const ROM: &str = "/home/toy/Games/test.gba";
const SAVE: &str = "/home/toy/Games/test.sav";
const SAVED: &[u8] = b"what gbae writes beside the ROM";
/// What the package says it saw as `game`, on one line the job reads.
const SAW: &str = "GAME ";

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some(APP) => app(),
        Some(GAME) => game(),
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
    granted(&mut red);
    for dir in [PACKAGE, ROW_PACKAGE, "/apps/other", "/home/toy/Apps/other", HOME, GAMES] {
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

/// `line` run by a shell this job launches, whose `login` row opens a login
/// session for what it launches: the session `grants` is answered in.
fn in_login(line: &str) -> std::process::Output {
    Command::new("/system/bin/shell").args(["-c", line]).current_dir("/").output().expect("launch a shell")
}

/// What the package saw as `game`: its working directory, the ROMs gbae's
/// menu lists there, and whether its save beside the first was written.
fn played(red: &mut Vec<String>, when: &str) -> Option<String> {
    let ran = Command::new(PROGRAM).arg(GAME).current_dir("/").output().expect("launch the package through the launcher");
    let out = String::from_utf8_lossy(&ran.stdout).into_owned();
    print!("{out}");
    match out.lines().find_map(|l| l.strip_prefix(SAW)) {
        Some(saw) if ran.status.code() == Some(0) => Some(saw.to_string()),
        _ => {
            red.push(format!("{when}: the package said nothing it saw: {ran:?}"));
            None
        }
    }
}

fn granted(red: &mut Vec<String>) {
    let _ = in_login("/system/bin/grants revoke appview");
    let _ = fs::remove_dir_all(GAMES);
    fs::create_dir_all(GAMES).expect("make the folder the package is granted");
    fs::write(ROM, b"a ROM's bytes").expect("put a ROM in it");

    let outside = Command::new("/system/bin/grants").args(["add", "appview", GAMES]).output().expect("launch grants");
    match (outside.status.code(), String::from_utf8_lossy(&outside.stdout)) {
        (Some(1), said) if said.contains("only a login session may ask for grants") => println!("  grants in the machine's session: refused"),
        _ => red.push(format!("grants in the machine's session was answered {outside:?}")),
    }
    let apps = in_login(&format!("/system/bin/grants add appview {HOME}"));
    match String::from_utf8_lossy(&apps.stdout) {
        said if said.contains("where every app keeps its own folder") => println!("  {HOME} as a grant: refused"),
        _ => red.push(format!("{HOME} as a grant was answered {apps:?}")),
    }
    let added = in_login(&format!("/system/bin/grants add appview {GAMES}"));
    if !String::from_utf8_lossy(&added.stdout).contains("appview is granted /home/toy/Games read-write") {
        red.push(format!("the grant was answered {added:?}"));
    }
    let listed = in_login("/system/bin/grants list");
    if !String::from_utf8_lossy(&listed.stdout).contains("appview read-write /home/toy/Games\n") {
        red.push(format!("grants list answered {listed:?}"));
    }

    let rom = format!("cwd={GAMES} roms=[\"test.gba\"] saved=Ok");
    match played(red, "granted") {
        Some(saw) if saw == rom => println!("  granted: {saw}"),
        Some(saw) => red.push(format!("granted, it saw {saw}, not {rom}")),
        None => {}
    }
    match fs::read(SAVE) {
        Ok(bytes) if bytes == SAVED => println!("  its save is beside the ROM"),
        other => red.push(format!("{SAVE} holds {other:?}")),
    }
    let _ = fs::remove_file(SAVE);

    // One byte more is another binary, which holds no grant.
    let mut longer = fs::read(SELF).expect("read this binary");
    longer.push(0);
    fs::write(PROGRAM, &longer).expect("install a binary one byte longer");
    let none = "cwd=/ roms=[] saved=Err";
    match played(red, "another binary") {
        Some(saw) if saw == none => println!("  another binary: {saw}"),
        Some(saw) => red.push(format!("another binary saw {saw}, not {none}")),
        None => {}
    }
    fs::copy(SELF, PROGRAM).expect("install this binary again");
    match played(red, "the granted binary again") {
        Some(saw) if saw == rom => println!("  the granted binary again: {saw}"),
        Some(saw) => red.push(format!("the granted binary again saw {saw}, not {rom}")),
        None => {}
    }
    let _ = fs::remove_file(SAVE);

    let revoked = in_login("/system/bin/grants revoke appview");
    if !String::from_utf8_lossy(&revoked.stdout).contains("appview's grant is revoked") {
        red.push(format!("the revoke was answered {revoked:?}"));
    }
    match played(red, "revoked") {
        Some(saw) if saw == none => println!("  revoked: {saw}"),
        Some(saw) => red.push(format!("revoked, it saw {saw}, not {none}")),
        None => {}
    }
    if fs::metadata(SAVE).is_ok() {
        red.push(format!("revoked, {SAVE} was written"));
    }
}

/// As gbae, started with no ROM: its menu on the working directory, by its
/// own `list_directory`, and a save beside the first ROM it lists, where gbae
/// puts one, or where the granted folder's would be with none listed.
fn game() {
    let cwd = std::env::current_dir().expect("a working directory");
    let entries = list_directory(&cwd);
    let roms: Vec<String> = entries.iter().filter(|e| !e.is_directory).map(|e| e.name.clone()).collect();
    let saved = match entries.iter().find(|e| !e.is_directory) {
        Some(rom) => {
            let rom = rom.path.canonicalize().unwrap_or_else(|_| rom.path.clone());
            fs::write(rom.with_extension("sav"), SAVED)
        }
        None => fs::write(SAVE, SAVED),
    };
    let saved = if saved.is_ok() { "Ok" } else { "Err" };
    println!("{SAW}cwd={} roms={roms:?} saved={saved}", cwd.display());
}

struct FileEntry {
    name: String,
    path: std::path::PathBuf,
    is_directory: bool,
}

/// gbae's own, verbatim (`Japabu/gbae` at `bfe8dab`, `src/menu.rs`).
fn list_directory(directory: &std::path::Path) -> Vec<FileEntry> {
    let mut directories = Vec::new();
    let mut roms = Vec::new();
    for entry in std::fs::read_dir(directory).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            directories.push(FileEntry {
                name: format!("{}/", name),
                path,
                is_directory: true,
            });
        } else if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("gba")) {
            roms.push(FileEntry { name, path, is_directory: false });
        }
    }
    let by_name = |a: &FileEntry, b: &FileEntry| a.name.to_lowercase().cmp(&b.name.to_lowercase());
    directories.sort_by(by_name);
    roms.sort_by(by_name);
    let parent = directory.parent().map(|parent| FileEntry {
        name: "../".to_string(),
        path: parent.to_path_buf(),
        is_directory: true,
    });
    parent.into_iter().chain(directories).chain(roms).collect()
}
