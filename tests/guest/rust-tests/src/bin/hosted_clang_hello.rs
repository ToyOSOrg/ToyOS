//! clang built for ToyOS compiles a C program here, and its driver links one,
//! starting `ld.lld` as a child; the program runs, and what each wrote is the
//! host's clang's bytes.
//!
//! Its one argument is the directory the harness staged on ROOT: `bin/clang`
//! and `bin/ld.lld` with clang's resource headers at `lib/clang`, the C
//! sysroot at `sysroot`, the program at `src/hello.c`, and what the host's
//! clang made of it at `host/hello.o` and `host/a.out`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where this job writes: `/tmp` is the guest's one writable tree.
const OUT: &str = "/tmp/hosted_clang_hello";

fn main() {
    let staged = PathBuf::from(std::env::args().nth(1).expect("the staged directory, as the one argument"));
    let sysroot = staged.join("sysroot");
    let out = Path::new(OUT);
    std::fs::create_dir_all(out).unwrap_or_else(|e| panic!("create {OUT}: {e}"));
    let (object, linked) = (out.join("hello.o"), out.join("a.out"));

    // The arguments are the host's, so the two products can be compared.
    let clang = |args: &[&str], to: &Path| {
        let mut command = Command::new(staged.join("bin/clang"));
        command
            .arg("--target=x86_64-unknown-toyos")
            .arg(format!("--sysroot={}", sysroot.display()))
            .args(args)
            .arg("hello.c")
            .arg("-o")
            .arg(to)
            .current_dir(staged.join("src"));
        command
    };
    run(clang(&["-c"], &object), "clang -c");
    // `clang hello.c`: the driver compiles, and links by starting `ld.lld`.
    run(clang(&[], &linked), "clang");

    let ran = Command::new(&linked).output().unwrap_or_else(|e| panic!("run {}: {e}", linked.display()));
    assert!(ran.status.success(), "{} ended {:?}: {}", linked.display(), ran.status, String::from_utf8_lossy(&ran.stderr));
    for line in String::from_utf8_lossy(&ran.stdout).lines() {
        println!("hosted_clang_hello: hello said: {line}");
    }

    let mut differ = false;
    for (made, by) in [(&object, "clang -c"), (&linked, "clang")] {
        let name = made.file_name().expect("a file name");
        differ |= !same(made, &staged.join("host").join(name), by);
    }
    assert!(!differ, "the guest's products are not the host's bytes");
    println!("hosted_clang_hello: ok");
}

/// Run `command`, and refuse its failure with what it said.
fn run(mut command: Command, what: &str) {
    let done = command.output().unwrap_or_else(|e| panic!("{what} did not start: {e}"));
    assert!(
        done.status.success(),
        "{what} ended {:?}:\n{}{}",
        done.status,
        String::from_utf8_lossy(&done.stdout),
        String::from_utf8_lossy(&done.stderr)
    );
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
