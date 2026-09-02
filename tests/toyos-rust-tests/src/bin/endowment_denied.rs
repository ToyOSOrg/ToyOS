//! What a process holds is what its parent gave it, and there is no second
//! place to ask.
//!
//! **The test estate is the one place least authority is not enforced** — a test
//! binary holds what `test-runner` holds, because the guest binaries are not
//! `[programs]` keys and no manifest row can name what any of them needs.
//! This binary is where least
//! authority *is* asserted, and it works because it builds its own namespaces
//! and spawns itself: nothing here depends on what test-runner handed over.
//!
//! Two halves.
//!
//! **Names.** One child is endowed a namespace carrying `echo` alone and another
//! the same namespace carrying `echo` and `privileged`. The first must resolve
//! `echo` and answer `NotFound` for `privileged`; the second must resolve both.
//! The second arm is the whole of what stops the first passing because the
//! service was never there.
//!
//! **Capabilities.** Five things are reachable no other way — minting a device
//! claim, entering the real-time band, turning a pid into a process handle,
//! reading the roster of every process in the machine, and powering the machine
//! off
//! — and each is one bit on a handle to a `SysCap` the kernel mints exactly once,
//! for `/bin/init`. A handle that carries the wrong *bit* is refused with a word,
//! because probing what an attenuated capability can still do is what
//! attenuation is for; a handle that is **no handle at all** ends the caller.
//!
//! **The roster arms are the ones whose subject is half a syscall.**
//! `SYS_SYSINFO` answers a machine header and then one entry per live thread,
//! and only the second is authority: the entries carry a pid, a size, a CPU
//! time and a **name** for every process there is, and they were ambient until
//! the owner ruled on 2026-08-20 that they ride `Rights::ROSTER`. Which of the
//! two a call is asking for is the buffer's own length, so the arms come in a
//! pair one byte apart — a buffer one byte short of an entry is answered
//! without the capability being consulted, and the same buffer one byte longer
//! is refused to the same handle. A kernel that demanded the bit for the header
//! fails the first; a kernel that stopped demanding it fails the second.
//!
//! `/bin/ps` is then run twice, endowed a duplicate with the bit and a
//! duplicate without it. The shipped applet reaching those same two answers
//! through the SDK is what says the manifest's name, the kernel's demand and
//! the program are one line rather than three that agree by luck — and it is
//! the only thing in this suite that runs `ps` at all.
//!
//! **The shutdown arms are the ones with a machine behind them.** `SYS_SHUTDOWN`
//! used to take no argument, so both of them made the call and the guest went
//! away: a kernel that stops demanding `Rights::POWER` does not fail these
//! assertions, it powers off the boot they run on, which is the loudest red
//! this suite can produce and exactly the defect being denied.
//!
//! **The applets nobody compared.** A row's granularity is the binary and
//! `/bin/toybox` is many programs behind many links, so every applet is endowed
//! the union; the last half holds each against a policy table.
//!
//! **A wrong-typed handle is refused with a word here, and that is a property of
//! the check rather than an exception to the policy.** The table resolves rights
//! before type, and `DEVICE`, `RT`, `POWER` and `ROSTER` are bits only a
//! `SysCap` ever carries — so
//! nothing of another type can reach the type check at all, and presenting one is
//! indistinguishable from presenting an attenuated capability. Asserted, because
//! it is the answer a caller gets and a test that expected the kill would be
//! asserting something the design cannot do.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::toyos::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos::system::{SYSINFO_ENTRY_SIZE, SYSINFO_HEADER_SIZE};
use toyos::{namespace, port, AsHandle};
use toyos_abi::handle::{Rights, HANDLE_INVALID};
use toyos_abi::syscall::{
    self, DeviceType, NamespaceBuild, SyscallError, NAMESPACE_FLAGS_KNOWN, NAMESPACE_KEEP_ALL,
    SVC_LABEL,
};
use toyos_abi::RawHandle;

const SELF_PATH: &str = "/bin/test_rs_endowment_denied";
const PS_PATH: &str = "/bin/ps";
const OPEN: &str = "echo";
const PRIVILEGED: &str = "privileged";

/// A `NamespaceBuild` flags bit nothing defines; the assert keeps it that way.
const UNDEFINED_FLAG: u32 = 1 << 31;
const _: () = assert!(UNDEFINED_FLAG & NAMESPACE_FLAGS_KNOWN == 0);

/// A buffer with room for one roster entry: the smallest question that costs
/// `Rights::ROSTER`, and one byte more than the largest that does not.
const ONE_ENTRY: usize = SYSINFO_HEADER_SIZE + SYSINFO_ENTRY_SIZE;

/// `process::HANDLE_FAULT_EXIT_CODE`.
const HANDLE_FAULT: i32 = 139;

/// The artifacts this image carries, not the `system.toml` that produced them.
const MANIFEST: &str = "/etc/system.manifest";
const BIN: &str = "/bin";
const MULTICALL: &str = "/bin/toybox";

/// What reaches a spawned process as authority rather than as an argument.
const AUTHORITY: &[&str] = &["serve", "provide", "receive", "device", "syscap"];

/// What each applet behind [`MULTICALL`] needs, as the manifest record that
/// would grant it. An applet with no row here is one nobody has spoken for.
const APPLET_NEEDS: &[(&str, &[&str])] = &[
    ("cat", &[]),
    ("cp", &[]),
    ("echo", &[]),
    ("free", &[]),
    ("grep", &[]),
    ("hexdump", &[]),
    ("ls", &[]),
    ("mkdir", &[]),
    ("mv", &[]),
    ("ps", &["syscap roster"]),
    ("pwd", &[]),
    ("rm", &[]),
    ("shutdown", &["syscap power"]),
    ("tone", &["receive soundd"]),
];

/// Every authority this image hands an applet that has no use for it: the exact
/// size of `issues/isolation/toybox-is-one-row-for-nineteen-applets.md` here. It
/// shrinks when the row is split per authority class and never grows.
const DECLARED_OVER_GRANTS: &[&str] = &[
    "cat: receive soundd",
    "cp: receive soundd",
    "echo: receive soundd",
    "free: receive soundd",
    "grep: receive soundd",
    "hexdump: receive soundd",
    "ls: receive soundd",
    "mkdir: receive soundd",
    "mv: receive soundd",
    "ps: receive soundd",
    "pwd: receive soundd",
    "rm: receive soundd",
    "shutdown: receive soundd",
];

/// Presenting no handle at all, each raised in a child of its own because the
/// kernel's answer to it is to end the caller.
const NOT_A_HANDLE: &[(&str, &str)] = &[
    ("claim-absent", "SYS_DEVICE_CLAIM took a handle nobody holds"),
    ("rt-absent", "SYS_RT_ENTER took a handle nobody holds"),
    // The third, and the only one whose failure mode is not a wrong exit code:
    // a kernel that took this handle would cut the power to the guest the
    // parent is waiting on.
    ("shutdown-absent", "SYS_SHUTDOWN took a handle nobody holds"),
    // The fourth. `HANDLE_INVALID` is exactly what a header-only caller sends,
    // and asking for one entry with it is the same mistake as the three above:
    // the census is not reachable by naming nothing.
    ("roster-absent", "SYS_SYSINFO's roster took a handle nobody holds"),
];

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("probe") => probe(),
        Some(role) => not_a_handle(role),
        None => test(),
    }
}

fn test() {
    only_what_was_given();
    the_base_plus_one_more_name();
    a_right_the_capability_lacks_is_a_word();
    the_roster_is_a_right_and_the_header_is_not();
    the_shipped_applet_reaches_both_answers();
    every_applet_holds_only_what_its_policy_names();
    for (role, what_would_be_wrong) in NOT_A_HANDLE {
        killed(role, what_would_be_wrong);
    }
    println!("a name outside the namespace resolves to nothing, and a capability is its bits");
}

/// The two namespaces, and the one difference between them.
fn only_what_was_given() {
    let (_open_acceptor, open) = port::create().expect("a port for the open service");
    let (_priv_acceptor, privileged) = port::create().expect("a port for the privileged service");

    let narrow = namespace::build().add(OPEN, &open).finish().expect("the narrow namespace");
    assert_eq!(
        probe_with(narrow.into_raw()),
        format!("{OPEN}=ok {PRIVILEGED}=NotFound"),
        "a child endowed one name reached the other",
    );

    let wide = namespace::build()
        .add(OPEN, &open)
        .add(PRIVILEGED, &privileged)
        .finish()
        .expect("the wide namespace");
    assert_eq!(
        probe_with(wide.into_raw()),
        format!("{OPEN}=ok {PRIVILEGED}=ok"),
        "a child endowed both names could not reach one of them",
    );
    println!("  names: the narrow child reached one, the wide child reached both");
}

/// Inheritance plus one more name, which only a flags bit can spell: no syscall
/// enumerates a namespace. The refusal arm is what stops the first from being a
/// bit the kernel merely tolerated.
fn the_base_plus_one_more_name() {
    let (_open_acceptor, open) = port::create().expect("a port for the open service");
    let (_priv_acceptor, privileged) = port::create().expect("a port for the privileged service");

    let base = namespace::build().add(OPEN, &open).finish().expect("the base namespace");
    // `keep` spells the same base by naming it, so the two spellings must reach
    // the same child.
    let listed = namespace::build()
        .keep(&base, &[OPEN])
        .add(PRIVILEGED, &privileged)
        .finish()
        .expect("the base's one name listed, plus one more");
    let by_name = probe_with(listed.into_raw());
    let merged = namespace::build()
        .keep_all(&base)
        .add(PRIVILEGED, &privileged)
        .finish()
        .expect("the whole base, plus one more name");
    assert_eq!(
        probe_with(merged.into_raw()),
        by_name,
        "keep_all and a keep list over the same base reached different children",
    );
    assert_eq!(
        by_name,
        format!("{OPEN}=ok {PRIVILEGED}=ok"),
        "a child given the base plus one name could not reach both",
    );

    let undefined = NamespaceBuild {
        base: base.as_handle(),
        flags: NAMESPACE_KEEP_ALL | UNDEFINED_FLAG,
        keep_ptr: 0,
        keep_n: 0,
        add_ptr: 0,
        add_n: 0,
        names_ptr: 0,
        names_len: 0,
    };
    // SAFETY: every length is zero, so no pointer above is read.
    let refused = unsafe { syscall::namespace_build(&undefined) };
    assert_eq!(
        refused.err(),
        Some(SyscallError::InvalidArgument),
        "a build carrying an undefined flags bit was not refused",
    );

    // The same request with the bit cleared, so what was refused is the bit.
    let defined = NamespaceBuild { flags: NAMESPACE_KEEP_ALL, ..undefined };
    // SAFETY: as above.
    let built = unsafe { syscall::namespace_build(&defined) }.expect("the request without the bit");
    syscall::close(built);
    println!("  keep_all: the whole base carried over, and an undefined flags bit was refused");
}

fn probe_with(ns: RawHandle) -> String {
    let mut child = Command::new(SELF_PATH)
        .arg("probe")
        .endow(SVC_LABEL, ns.0)
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the probe");
    let mut out = BufReader::new(child.stdout.take().expect("probe stdout"));
    let mut line = String::new();
    out.read_line(&mut line).expect("the probe's report");
    assert!(child.wait().expect("wait the probe").success(), "the probe exited nonzero");
    line.trim().to_string()
}

/// A capability handle that resolves and does not carry the bit.
///
/// The test estate's own cap is `DEVICE | DUP`, so the RT arm needs no
/// narrowing at all — this binary genuinely cannot enter the real-time band,
/// which is the privilege a device claim was never enough to confer.
fn a_right_the_capability_lacks_is_a_word() {
    let cap = cap();

    assert_eq!(
        syscall::rt_enter(cap.as_handle()),
        Err(SyscallError::PermissionDenied),
        "a capability without RT entered the real-time band",
    );

    let toothless = cap.narrowed(Rights::DUP).expect("a capability carrying less");
    assert_eq!(
        syscall::device_claim(toothless.as_handle(), DeviceType::Keyboard).err(),
        Some(SyscallError::PermissionDenied),
        "a capability without DEVICE minted a claim",
    );

    // **Narrowed and not the estate's own cap, because this estate does carry
    // `POWER`.** `run shutdown` is how a dozen host-side gates end their guest,
    // so `tests/testcases` names `power` on the test-runner row and every
    // binary it spawns holds a duplicate — including this one. The subject is a
    // capability that resolves and lacks the bit, which is what `toothless` is.
    //
    // There is no arm for the unnarrowed cap here, and there cannot be: the
    // call that proves the estate *does* hold `POWER` does not come back, and
    // `run shutdown` at the end of a dozen host-side gates is that proof.
    assert_eq!(
        toothless.shutdown(),
        SyscallError::PermissionDenied,
        "a capability without POWER shut the machine down",
    );

    // A handle that is not a capability at all. It never reaches the type
    // check: `DEVICE` and `RT` are bits nothing else carries, so this is the
    // same refusal the narrowed capability got.
    assert_eq!(
        syscall::device_claim(RawHandle(1), DeviceType::Keyboard).err(),
        Some(SyscallError::PermissionDenied),
        "a pipe was taken as a capability by SYS_DEVICE_CLAIM",
    );
    assert_eq!(
        syscall::rt_enter(RawHandle(1)),
        Err(SyscallError::PermissionDenied),
        "a pipe was taken as a capability by SYS_RT_ENTER",
    );
    assert_eq!(
        syscall::shutdown(RawHandle(1)),
        SyscallError::PermissionDenied,
        "a pipe was taken as a capability by SYS_SHUTDOWN",
    );

    // And the unnarrowed one does carry `DEVICE`, so the refusals above were
    // the bit and not the call. What it answers beyond that is a fact about
    // the machine — `NotFound` for a class this boot has no driver for — and
    // the assertion says only that it is not the refusal. A claim is released
    // as soon as it is taken, because a claim moves and the boot runs other
    // binaries that need this one.
    let with_the_bit = syscall::device_claim(cap.as_handle(), DeviceType::Keyboard);
    assert_ne!(
        with_the_bit.err(),
        Some(SyscallError::PermissionDenied),
        "the estate's capability does not carry DEVICE either, so the refusal above proved nothing",
    );
    if let Ok(claim) = with_the_bit {
        syscall::close(claim);
    }
    println!("  capability: refused for the bit it lacks and for having none, allowed for the bit it has");
    println!("  power: a capability without POWER, and a pipe, were both refused the machine");
}

/// The estate's system capability, taken once — taking an endowment is a swap,
/// and three arms below want the same one.
fn cap() -> &'static SysCap {
    static CAP: OnceLock<SysCap> = OnceLock::new();
    CAP.get_or_init(|| {
        Endowments::get()
            .take(SYSCAP_LABEL)
            .expect("test-runner endows every binary it spawns a system capability")
    })
}

/// `SYS_SYSINFO`'s two answers, and the one byte of buffer between them.
///
/// **The pair is the point.** One capability, two calls that differ only in
/// whether the buffer has room for a single entry: the shorter is answered and
/// the longer is refused. A kernel that demanded the bit unconditionally would
/// fail the first and break `free`, netd and the compositor's taskbar with it;
/// a kernel that stopped demanding it — the tree as it stood before the owner's
/// ruling of 2026-08-20 — fails the second.
///
/// Narrowed rather than the estate's own cap, because this estate does carry
/// `ROSTER`: four guest binaries read entries out of the same call, and
/// `tests/testcases` names `roster` on the test-runner row for them. The
/// subject is a capability that resolves and lacks the bit.
fn the_roster_is_a_right_and_the_header_is_not() {
    let toothless = cap().narrowed(Rights::TRANSFER).expect("a capability carrying less");

    let mut short = [0u8; ONE_ENTRY - 1];
    assert_eq!(
        toothless.roster(&mut short),
        SYSINFO_HEADER_SIZE,
        "a buffer one byte short of an entry was not answered the machine header",
    );
    assert_ne!(
        u64::from_le_bytes(short[0..8].try_into().unwrap()),
        0,
        "the header came back saying this machine has no memory",
    );

    let mut one = [0u8; ONE_ENTRY];
    assert_eq!(
        toothless.roster(&mut one),
        0,
        "a capability without ROSTER was given the process roster",
    );
    // The demand is above the header write, so a refusal leaves the buffer
    // exactly as it was — not a header with the entries withheld.
    assert!(
        one.iter().all(|&b| b == 0),
        "a refused roster wrote {} bytes into the caller's buffer",
        one.iter().filter(|&&b| b != 0).count(),
    );

    // A handle that is not a capability at all, asking for an entry. `ROSTER`
    // is a bit nothing else carries, so it never reaches the type check and the
    // answer is the word the narrowed capability got — the same shape the three
    // arms above assert for `DEVICE`, `RT` and `POWER`.
    let mut by_a_console = [0u8; ONE_ENTRY];
    assert_eq!(
        syscall::sysinfo(RawHandle(1), &mut by_a_console),
        0,
        "a console was taken as a capability by SYS_SYSINFO",
    );

    // The header with no handle named at all — `HANDLE_INVALID`, which is what
    // `toyos::system::sysinfo` sends and what every header-only reader in the
    // tree is. Ambient by the owner's ruling, and this is where that half is
    // asserted.
    let mut header = [0u8; SYSINFO_HEADER_SIZE];
    assert_eq!(
        toyos::system::sysinfo(&mut header),
        SYSINFO_HEADER_SIZE,
        "the machine header stopped being ambient",
    );

    // And the estate's own capability does carry the bit, so the refusals above
    // were the bit and not the call — with this process's own entry in the
    // answer, which is what says entries were written rather than counted.
    let mut wide = vec![0u8; SYSINFO_HEADER_SIZE + SYSINFO_ENTRY_SIZE * 256];
    let n = cap().roster(&mut wide);
    assert!(n >= ONE_ENTRY, "a capability carrying ROSTER was answered {n} bytes");
    let me = syscall::getpid().raw();
    let mine = (SYSINFO_HEADER_SIZE..)
        .step_by(SYSINFO_ENTRY_SIZE)
        .take_while(|pos| pos + SYSINFO_ENTRY_SIZE <= n)
        .any(|pos| u32::from_le_bytes(wide[pos..pos + 4].try_into().unwrap()) == me);
    assert!(mine, "the roster this process was given does not contain this process");

    println!("  roster: one byte short is the header and ambient, one entry costs ROSTER");
}

/// `/bin/ps`, endowed a duplicate with the bit and a duplicate without it.
///
/// **The shipped applet and not a raw call, because the plumbing is the risk.**
/// The manifest's `roster` name, `syscap_rights`' bit, init's narrowing, the
/// SDK's `SysCap::roster` and the kernel's demand are five places that have to
/// agree, and every arm above this one exercises the last of them through the
/// first only by hand. This runs the program a user runs. It is also the only
/// thing in this suite that runs `ps` at all.
fn the_shipped_applet_reaches_both_answers() {
    let granted = ps_endowed(
        cap().narrowed(Rights::TRANSFER.union(Rights::ROSTER)).expect("a cap carrying ROSTER"),
    );
    let said = String::from_utf8_lossy(&granted.stdout);
    assert_eq!(granted.status.code(), Some(0), "ps with ROSTER exited nonzero: {said}");
    let mut lines = said.lines();
    let header = lines.next().unwrap_or("");
    assert!(
        header.contains("PID") && header.contains("NAME"),
        "ps with ROSTER did not print its column header: {said:?}",
    );
    let rows = lines.count();
    assert!(rows > 0, "ps with ROSTER printed a header and no process: {said:?}");

    let refused = ps_endowed(cap().narrowed(Rights::TRANSFER).expect("a cap carrying less"));
    let complaint = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(
        refused.status.code(),
        Some(1),
        "ps without ROSTER did not refuse: {:?} / {complaint:?}",
        String::from_utf8_lossy(&refused.stdout),
    );
    assert!(
        complaint.contains("ROSTER"),
        "ps without ROSTER did not say which bit it lacked: {complaint:?}",
    );
    assert!(
        refused.stdout.is_empty(),
        "ps without ROSTER printed a roster anyway: {:?}",
        String::from_utf8_lossy(&refused.stdout),
    );

    // The rows do not sum to the machine, and `ps` says by how much rather than
    // leaving the difference to be read as unattributed CPU.
    assert!(
        said.lines().last().is_some_and(|l| l.contains("have been reaped")),
        "ps accounted for no reaped CPU at all: {said:?}",
    );

    println!("  ps: {rows} processes with the bit, and a named refusal without it");
}

/// The authority records each `program` row grants, keyed by the binary path
/// its own line names. **Its own parse, not `toyos_manifest`'s**: a checker that
/// reads the manifest through init's parser and resolves links through init's
/// resolver agrees with init by construction.
fn manifest_rows(text: &str) -> BTreeMap<String, Vec<String>> {
    let mut rows: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current = String::new();
    for line in text.lines() {
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        if key == "program" {
            let (_name, path) = rest.split_once(' ').unwrap_or_else(|| {
                panic!("a `program` line names a name and a path: {line:?}")
            });
            current = path.to_string();
            rows.entry(current.clone()).or_default();
        } else if AUTHORITY.contains(&key) {
            let row = rows.get_mut(&current).unwrap_or_else(|| {
                panic!("a `{key}` record before any `program` line: {line:?}")
            });
            row.push(format!("{key} {rest}"));
        }
    }
    rows
}

/// Every applet behind one binary, held against what each of them needs.
///
/// **The two sides come from different places on purpose**: the links off the
/// image, the rows off the rendered manifest, neither through `declared`, and
/// the policy from [`APPLET_NEEDS`]. The arms above are the
/// allowed-and-forbidden pair for every class this finds.
fn every_applet_holds_only_what_its_policy_names() {
    let manifest = std::fs::read_to_string(MANIFEST).expect("the image carries its own manifest");
    let rows = manifest_rows(&manifest);

    let mut applets: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(BIN).expect("read /bin") {
        let path = entry.expect("a /bin entry").path();
        let meta = std::fs::symlink_metadata(&path).expect("lstat a /bin entry");
        if !meta.file_type().is_symlink() {
            continue;
        }
        let target = std::fs::read_link(&path).expect("read a /bin link");
        assert_eq!(
            target.to_str(),
            Some(MULTICALL),
            "{path:?} is a link to something else: a second multicall binary needs its own \
             policy table, not this one",
        );
        applets.push(path.file_name().expect("a link has a name").to_string_lossy().into_owned());
    }
    applets.sort();
    assert!(!applets.is_empty(), "no applet link in {BIN}: the inventory side read nothing");

    let granted = rows
        .get(MULTICALL)
        .unwrap_or_else(|| panic!("{MULTICALL} has no row in {MANIFEST}"));
    let mut over = Vec::new();
    for applet in &applets {
        let (_, needs) = APPLET_NEEDS
            .iter()
            .find(|(name, _)| name == applet)
            .unwrap_or_else(|| panic!("`{applet}` is behind {MULTICALL} and no policy row says \
                                      what it needs"));
        for record in granted {
            if !needs.contains(&record.as_str()) {
                over.push(format!("{applet}: {record}"));
            }
        }
    }

    assert_eq!(
        over, DECLARED_OVER_GRANTS,
        "the authority {MULTICALL}'s one row hands applets that have no use for it is not the \
         list this test declares — a row was split, or one grew",
    );
    println!(
        "  applets: {} links behind {MULTICALL}, {} declared over-grants and no undeclared one",
        applets.len(),
        over.len(),
    );
}

/// Run `/bin/ps` holding exactly `cap` and nothing else of ours.
///
/// A fresh duplicate per run: the endowment moves into the child, and a claim
/// this process kept would be a second holder of one handle.
fn ps_endowed(cap: SysCap) -> Output {
    Command::new(PS_PATH)
        .endow(SYSCAP_LABEL, cap.into_raw().0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn /bin/ps")
        .wait_with_output()
        .expect("wait for /bin/ps")
}

/// Run `role` and require that the kernel ended it at its call.
///
/// The marker is what gives the arm teeth: a child that died before reaching
/// the call would otherwise pass while asserting nothing.
fn killed(role: &str, what_would_be_wrong: &str) {
    let child = Command::new(SELF_PATH)
        .arg(role)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {role}: {e}"));
    let out = child.wait_with_output().unwrap_or_else(|e| panic!("wait {role}: {e}"));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("reached {role}"),
        "{role} never reached its call",
    );
    assert_eq!(out.status.code(), Some(HANDLE_FAULT), "{what_would_be_wrong}");
    println!("  {role}: ended the caller");
}

fn probe() {
    let answer = |name: &str| match toyos::endow::namespace().map(|ns| ns.open(name)) {
        Some(Ok(_)) => "ok".to_string(),
        Some(Err(e)) => format!("{e:?}"),
        None => "no namespace".to_string(),
    };
    println!("{OPEN}={} {PRIVILEGED}={}", answer(OPEN), answer(PRIVILEGED));
    std::io::stdout().flush().expect("probe: flush");
}

fn not_a_handle(role: &str) -> ! {
    assert!(NOT_A_HANDLE.iter().any(|(name, _)| *name == role), "unknown role {role:?}");
    println!("reached {role}");
    std::io::stdout().flush().expect("flush the marker");
    let answered = match role {
        "claim-absent" => {
            format!("{:?}", syscall::device_claim(HANDLE_INVALID, DeviceType::Keyboard))
        }
        "rt-absent" => format!("{:?}", syscall::rt_enter(HANDLE_INVALID)),
        // `HANDLE_INVALID` with room for one entry. The same number is the
        // ordinary, correct argument for a header-only call — which is why this
        // arm exists: the difference between the two is the buffer, and a
        // kernel that read the handle for both would kill every `free` in the
        // machine instead.
        "roster-absent" => {
            let mut buf = [0u8; ONE_ENTRY];
            format!("{} bytes", syscall::sysinfo(HANDLE_INVALID, &mut buf))
        }
        // If this comes back at all the kernel refused it, which is already the
        // wrong answer for a handle nobody holds. If it does not come back, the
        // machine this child is running on has been powered off and the parent
        // never reads a word of this.
        _ => format!("{:?}", syscall::shutdown(HANDLE_INVALID)),
    };
    panic!("{role} was answered {answered} instead of ending the caller");
}
