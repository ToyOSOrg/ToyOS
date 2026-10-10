//! A package's launch from the desktop asks the person at the screen for a
//! folder, in the prompt layer, and what they answer is what it holds.
//!
//! Run by `consent_prompt` on `tests/consentcase`, whose host side drives the
//! desktop through QEMU's keyboard and tablet: each step here says
//! `CONSENT <step>` on the console, the host clicks the package in the
//! launcher and answers the prompt with keys, and taps F12 once the package
//! has said what it saw (`CONSENTEE ...`). The tap reaches the job through
//! the one window it holds:
//!
//! - **a hostile client**, this binary as `hostile`: a fullscreen topmost
//!   window that says every key and press the compositor gives it. It may be
//!   given F12 and nothing else: every other key the host types, and the press
//!   it makes outside the prompt, are the prompt's or nobody's.
//!
//! The steps, each held on the host to what the package saw and to the
//! store `/state/supervisor/grants`, which the job prints:
//!
//! - `always`: Games, Always. Stored; it lists the ROM and saves beside it.
//! - `again`: no question; the same folder.
//! - `deny`, after a revoke: Deny. Stored; no folder.
//! - `denied`: no question; no folder.
//! - `skip`, after a revoke: Escape. Nothing stored; no folder.
//! - `once`: asked again; Games, Allow once. Nothing stored; the folder.
//! - `ssh`: the package started from a login session no screen's row opened
//!   asks nothing and holds no folder.
//!
//! First, std's half of the hash being of what runs: a prepared spawn starts
//! the bytes its prepare read, past a later working directory and a rewrite
//! of the file ([`prepared`]); and a grant of a file is refused and never
//! kept ([`not_a_folder`]).
//!
//! The package is this binary installed as `consentee`, which, started under
//! that name, does what `app_view`'s `game` does: list the ROMs in its
//! working directory and save beside the first.

use std::fs;
use std::io::{BufRead, BufReader, Lines, Write};
use std::process::{ChildStdout, Command, Stdio};

use toyos_desktop::Chrome;
use window::{Color, Event, ResolutionInfo, Window, MSG_GET_RESOLUTION, MSG_RESOLUTION_CHANGED};

const SELF: &str = "/system/bin/test_rs_consent";
const PACKAGE: &str = "/apps/consentee";
const PROGRAM: &str = "/apps/consentee/consentee";
const MANIFEST: &[u8] = b"name = \"consentee\"\nversion = \"1\"\n\
    digest = \"0000000000000000000000000000000000000000000000000000000000000000\"\n\
    program = \"/apps/consentee/consentee\"\n";
const HOME: &str = "/home/toy";
const GAMES: &str = "/home/toy/Games";
const ROM: &str = "/home/toy/Games/test.gba";
const SAVE: &str = "/home/toy/Games/test.sav";
const STORE: &str = "/state/supervisor/grants";
/// The hostile window's colour, which the host reads the panel for.
const HOSTILE: Color = Color { r: 0xff, g: 0x00, b: 0xff };
/// The key the host taps once a step is done: HID usage F12.
const DONE: u8 = 0x45;
/// What this binary says run as `bytes`.
const RAN: &str = "BYTES ran";

fn main() {
    let named = std::env::args().next().unwrap_or_default();
    if named.ends_with("/consentee") {
        return game();
    }
    match std::env::args().nth(1).as_deref() {
        Some("hostile") => hostile(),
        Some("bytes") => println!("{RAN}"),
        _ => job(),
    }
}

fn job() {
    let _ = fs::remove_dir_all(PACKAGE);
    let _ = fs::remove_dir_all(GAMES);
    fs::create_dir_all(PACKAGE).expect("make the package's directory");
    fs::copy(SELF, PROGRAM).expect("install this binary as the package's program");
    fs::write(format!("{PACKAGE}/manifest.toml"), MANIFEST).expect("write the package's manifest");
    fs::create_dir_all(GAMES).expect("make the folder the package is granted");
    fs::write(ROM, b"a ROM's bytes").expect("put a ROM in it");
    let _ = in_login("/system/bin/grants revoke consentee");

    let mut red = Vec::new();
    prepared(&mut red);
    not_a_folder(&mut red);
    // Every step below asks of an empty store, and waits on the host.
    if !red.is_empty() {
        panic!("consent: {red:#?}");
    }
    let mut hostile = Command::new(SELF)
        .arg("hostile")
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the hostile window");
    let mut said = BufReader::new(hostile.stdout.take().expect("its stdout")).lines();
    until(&mut said, &mut red, "HOSTILE up");

    // Where Games is in the prompt's list: the home's folders a package may be
    // granted, sorted as the prompt sorts them.
    let mut folders: Vec<String> = fs::read_dir(HOME)
        .expect("list the home")
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "Apps")
        .collect();
    folders.sort_by_key(|name| name.to_lowercase());
    let games = folders.iter().position(|name| name == "Games").expect("Games is in the home");

    step(&mut said, &mut red, &format!("always down={games}"));
    holds("always");
    match fs::read(SAVE) {
        Ok(_) => println!("  always: its save is beside the ROM"),
        Err(e) => red.push(format!("always: {SAVE}: {e}")),
    }
    let _ = fs::remove_file(SAVE);
    step(&mut said, &mut red, "again");
    let _ = fs::remove_file(SAVE);

    revoke(&mut red);
    step(&mut said, &mut red, "deny");
    holds("deny");
    step(&mut said, &mut red, "denied");

    revoke(&mut red);
    step(&mut said, &mut red, "skip");
    holds("skip");
    step(&mut said, &mut red, &format!("once down={games}"));
    holds("once");
    let _ = fs::remove_file(SAVE);

    println!("CONSENT ssh");
    let ran = in_login(PROGRAM);
    let out = String::from_utf8_lossy(&ran.stdout);
    let none = "CONSENTEE cwd=/ roms=[] saved=Err";
    match out.lines().find(|l| l.starts_with("CONSENTEE ")) {
        Some(saw) if saw == none => println!("  ssh: it saw {}", &saw["CONSENTEE ".len()..]),
        other => red.push(format!("from a login session off the screen it said {other:?}, not {none:?}: {ran:?}")),
    }
    println!("CONSENT ssh done");
    // Its own sentinel: the host taps once it has read the panel again.
    until_done(&mut said, &mut red, "ssh");

    let _ = hostile.kill();
    let _ = hostile.wait();
    let _ = in_login("/system/bin/grants revoke consentee");
    let _ = fs::remove_dir_all(PACKAGE);
    let _ = fs::remove_dir_all(GAMES);
    if !red.is_empty() {
        panic!("consent: {red:#?}");
    }
    println!("consent: every answer was the person's, and the package held what it was given");
}

/// **The bytes a prepared spawn starts are the ones its prepare read**, which
/// is what lets the supervisor hold a package to the hash of what it starts:
/// a working directory set after the prepare keeps them, and the file
/// rewritten after it, here into no program at all, is not read again.
fn prepared(red: &mut Vec<String>) {
    use std::os::toyos::process::CommandExt;
    let copy = format!("{GAMES}/prepared");
    let ours = fs::read(SELF).expect("read this binary");
    fs::write(&copy, &ours).expect("copy this binary onto DATA");
    let mut command = Command::new(&copy);
    command.arg("bytes");
    if let Err(e) = command.prepare() {
        red.push(format!("{copy} would not prepare: {e}"));
        return;
    }
    let read = command.prepared_image().map(<[u8]>::to_vec);
    fs::write(&copy, b"no program").expect("rewrite the copy");
    if let Err(e) = command.current_dir(HOME).prepare() {
        red.push(format!("{copy} would not prepare again in {HOME}: {e}"));
        return;
    }
    let kept = command.prepared_image().map(<[u8]>::to_vec);
    match (read.as_deref() == Some(&ours[..]), kept == read, command.output()) {
        (true, true, Ok(ran)) if String::from_utf8_lossy(&ran.stdout).contains(RAN) => {
            println!("  a prepared spawn started the bytes it read, past a rewrite")
        }
        (read, kept, ran) => red.push(format!(
            "a prepared spawn: the bytes read were this binary: {read}; kept across the directory: {kept}; it ran {ran:?}"
        )),
    }
    let _ = fs::remove_file(&copy);
}

/// **A grant is kept only of a folder**: one of a file the folder rules
/// admit is refused by name and nothing is stored. The supervisor holds the
/// person's Always to the same check, in the one function that stores both.
fn not_a_folder(red: &mut Vec<String>) {
    let note = format!("{HOME}/Note");
    fs::write(&note, b"a file, not a folder").expect("write a file in the home");
    let out = in_login(&format!("/system/bin/grants add consentee {note}"));
    let said = String::from_utf8_lossy(&out.stdout);
    let refused = format!("{note} is no directory");
    let kept = fs::read_to_string(STORE).unwrap_or_default().lines().any(|l| l.contains(" consentee "));
    match (said.contains(&refused), kept) {
        (true, false) => println!("  a grant of a file was refused and nothing was kept"),
        _ => red.push(format!("a grant of {note}: the supervisor said {said:?}, and the store holds a line for it: {kept}")),
    }
    let _ = fs::remove_file(&note);
}

/// Ask the host for `step`, and wait for its F12.
fn step(said: &mut Lines<BufReader<ChildStdout>>, red: &mut Vec<String>, step: &str) {
    println!("CONSENT {step}");
    let _ = std::io::stdout().flush();
    until_done(said, red, step);
}

/// Every line the hostile window says until the F12 the host taps, each held
/// to: F12 alone, and no press.
fn until_done(said: &mut Lines<BufReader<ChildStdout>>, red: &mut Vec<String>, step: &str) {
    let done = format!("HOSTILE key {DONE:#x} down");
    loop {
        let Some(Ok(line)) = said.next() else {
            red.push(format!("{step}: the hostile window ended"));
            return;
        };
        if line == done {
            return;
        }
        let allowed = line.starts_with(&format!("HOSTILE key {DONE:#x} "))
            || line.starts_with(&format!("HOSTILE mouse {} ", window::MOUSE_RELEASE))
            || line.starts_with(&format!("HOSTILE mouse {} ", window::MOUSE_MOVE));
        if !allowed {
            red.push(format!("{step}: the hostile window was given `{line}`"));
        }
    }
}

fn until(said: &mut Lines<BufReader<ChildStdout>>, red: &mut Vec<String>, want: &str) {
    match said.next() {
        Some(Ok(line)) if line == want => {}
        other => red.push(format!("the hostile window said {other:?}, not {want:?}")),
    }
}

/// The store's line for the package after `step`, for the host to hold to
/// the SHA-256 of the binary it put on ROOT, which nothing in the guest
/// computed.
fn holds(step: &str) {
    let store = fs::read_to_string(STORE).unwrap_or_default();
    let line = store.lines().find(|l| l.contains(" consentee ")).unwrap_or("");
    println!("STORE {step}: {line}");
}

fn revoke(red: &mut Vec<String>) {
    let out = in_login("/system/bin/grants revoke consentee");
    if !String::from_utf8_lossy(&out.stdout).contains("consentee's grant is revoked") {
        red.push(format!("the revoke was answered {out:?}"));
    }
}

/// `line` run by a shell this job launches, whose `login` row opens a login
/// session that is not the screen's.
fn in_login(line: &str) -> std::process::Output {
    Command::new("/system/bin/shell").args(["-c", line]).current_dir("/").output().expect("launch a shell")
}

/// A fullscreen window that stays on top and says everything it is given.
fn hostile() {
    let conn = toyos::endow::service("compositor").expect("a compositor");
    conn.signal(MSG_GET_RESOLUTION).expect("ask the compositor its resolution");
    let (kind, screen) = conn.recv::<ResolutionInfo>().expect("its resolution");
    assert_eq!(kind, MSG_RESOLUTION_CHANGED);
    let chrome = Chrome::DEFAULT;
    let (w, h) = (
        screen.width - chrome.chrome_w() as u32,
        screen.height - chrome.taskbar as u32 - chrome.chrome_h() as u32,
    );
    let mut window = Window::create_topmost(w, h, "hostile").expect("a topmost window");
    let fb = window.framebuffer();
    fb.clear(HOSTILE);
    window.present();
    loop {
        match window.recv_event() {
            Event::Frame => break,
            Event::Close => return,
            _ => {}
        }
    }
    println!("HOSTILE up");
    loop {
        match window.recv_event() {
            Event::KeyInput(key) => {
                println!("HOSTILE key {:#x} {}", key.keycode, if key.pressed() { "down" } else { "up" })
            }
            Event::MouseInput(mouse) => println!("HOSTILE mouse {} {} {}", mouse.event_type, mouse.x, mouse.y),
            Event::Close => return,
            _ => {}
        }
    }
}

/// As gbae, started with no ROM: the ROMs in the working directory, and a
/// save beside the first, or where the granted folder's would be.
fn game() {
    let cwd = std::env::current_dir().expect("a working directory");
    let mut roms: Vec<String> = fs::read_dir(&cwd)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".gba"))
        .collect();
    roms.sort();
    let save = match roms.first() {
        Some(rom) => cwd.join(rom).with_extension("sav"),
        None => SAVE.into(),
    };
    let saved = if fs::write(save, b"a save").is_ok() { "Ok" } else { "Err" };
    println!("CONSENTEE cwd={} roms={roms:?} saved={saved}", cwd.display());
}
