//! clang built for ToyOS, installed by `pkg`, compiles a C program here, and
//! its driver links one, starting `ld.lld` as a child; the program runs, and
//! what each wrote is the host's clang's bytes.
//!
//! Its arguments are the directory the harness staged on ROOT and the package
//! file in it: the package beside its `SHA256SUMS`, the program at
//! `src/hello.c`, and what the host's clang made of it at `host/hello.o` and
//! `host/a.out`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where this job writes: `/tmp` is the guest's one writable tree outside the
/// package.
const OUT: &str = "/tmp/hosted_clang_hello";

/// The installed toolchain's `bin`.
const BIN: &str = "/apps/clang/bin";

fn main() {
    let mut args = std::env::args().skip(1);
    let staged = PathBuf::from(args.next().expect("the staged directory, as the first argument"));
    let package = staged.join(args.next().expect("the package's file name, as the second argument"));
    let mut install = Command::new("/system/bin/pkg");
    install.arg("install").arg(&package).arg("--yes");
    run(install, "pkg install");

    let out = Path::new(OUT);
    std::fs::create_dir_all(out).unwrap_or_else(|e| panic!("create {OUT}: {e}"));
    let (object, linked) = (out.join("hello.o"), out.join("a.out"));

    // The arguments are the host's but its target and sysroot, which are the
    // installed clang's own: its default target, and the sysroot its
    // configuration file names.
    let compile = |driver: &str, args: &[&str], to: &Path| {
        let mut command = Command::new(Path::new(BIN).join(driver));
        command.args(args).arg("hello.c").arg("-o").arg(to).current_dir(staged.join("src"));
        command
    };
    run(compile("clang", &["-c"], &object), "clang -c");
    // `cc hello.c`: the driver by the link the package carries for it,
    // compiling, and linking by starting `ld.lld`.
    run(compile("cc", &[], &linked), "cc");

    let ran = Command::new(&linked).output().unwrap_or_else(|e| panic!("run {}: {e}", linked.display()));
    assert!(ran.status.success(), "{} ended {:?}: {}", linked.display(), ran.status, String::from_utf8_lossy(&ran.stderr));
    for line in String::from_utf8_lossy(&ran.stdout).lines() {
        println!("hosted_clang_hello: hello said: {line}");
    }

    let mut differ = false;
    for (made, by) in [(&object, "clang -c"), (&linked, "cc")] {
        let name = made.file_name().expect("a file name");
        differ |= !same(made, &staged.join("host").join(name), by);
    }
    assert!(!differ, "the guest's products are not the host's bytes");
    println!("hosted_clang_hello: ok");
}

/// Run `command`, say what it printed, and refuse its failure.
fn run(mut command: Command, what: &str) {
    let done = command.output().unwrap_or_else(|e| panic!("{what} did not start: {e}"));
    for line in String::from_utf8_lossy(&done.stdout).lines() {
        println!("hosted_clang_hello: {what}: {line}");
    }
    assert!(done.status.success(), "{what} ended {:?}:\n{}", done.status, String::from_utf8_lossy(&done.stderr));
    println!("hosted_clang_hello: {what} ended 0");
}

/// Whether `made` holds the bytes of `host`, saying so, and where they first
/// part if they do not.
fn same(made: &Path, host: &Path, by: &str) -> bool {
    let read = |p: &Path| std::fs::read(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
    let (ours, theirs) = (read(made), read(host));
    if ours == theirs {
        println!("hosted_clang_hello: {by}'s {} is the host's {} bytes", made.display(), ours.len());
        return true;
    }
    let at = ours.iter().zip(&theirs).position(|(a, b)| a != b).unwrap_or(ours.len().min(theirs.len()));
    let window = |bytes: &[u8]| bytes[at..bytes.len().min(at + 32)].iter().map(|b| format!("{b:02x}")).collect::<String>();
    println!(
        "hosted_clang_hello: {by}'s {} is {} bytes and the host's {}; they first part at {at:#x}: here {}, the host's {}",
        made.display(),
        ours.len(),
        theirs.len(),
        window(&ours),
        window(&theirs)
    );
    false
}
