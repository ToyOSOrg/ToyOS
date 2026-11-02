//! The shared SDK cwd is bounded, and short getcwd buffers are never truncated.

use std::fs;
use toyos::files;
use toyos_abi::syscall::MAX_PATH;

const DIR: &str = "/tmp/abuse_cwd";

fn cwd() -> Vec<u8> {
    let mut bytes = vec![0; files::getcwd(&mut [])];
    assert_eq!(files::getcwd(&mut bytes), bytes.len());
    bytes
}

fn refused_growth() {
    let before = cwd();
    for huge in ["a".repeat(60_000), "b/".repeat(30_000)] {
        assert!(fs::create_dir(&huge).is_err(), "mkdir accepted an overlong path");
        assert!(std::env::set_current_dir(&huge).is_err(), "chdir accepted an overlong path");
        assert_eq!(cwd(), before, "refused chdir moved cwd");
    }
}

fn legal_nesting() {
    let root = format!("{DIR}/deep");
    fs::create_dir(&root).expect("create deep root");
    std::env::set_current_dir(&root).expect("chdir deep root");
    for _ in 0..32 {
        let name = "n".repeat(64);
        fs::create_dir(&name).expect("create legal nested directory");
        std::env::set_current_dir(&name).expect("chdir legal nested directory");
    }
    let truth = cwd();
    assert!(truth.len() > 1024 && truth.len() <= MAX_PATH);
    assert_eq!(std::env::current_dir().expect("long current_dir").to_str().expect("UTF-8 cwd").as_bytes(), truth);

    let mut small = vec![0xaa; truth.len() - 1];
    assert_eq!(files::getcwd(&mut small), truth.len());
    assert!(small.iter().all(|&b| b == 0xaa), "getcwd wrote a partial path");
    let mut exact = vec![0; truth.len()];
    assert_eq!(files::getcwd(&mut exact), truth.len());
    assert_eq!(exact, truth);
    refused_growth();
}

fn main() {
    fs::create_dir(DIR).expect("create cwd test root");
    std::env::set_current_dir(DIR).expect("enter cwd test root");
    refused_growth();
    legal_nesting();

    std::env::set_current_dir(DIR).expect("return to test root");
    fs::write("probe.txt", b"resolved").expect("relative write");
    assert_eq!(fs::read(format!("{DIR}/probe.txt")).expect("absolute read"), b"resolved");
    std::env::set_current_dir("/").expect("return to root");
    fs::remove_dir_all(DIR).expect("clean cwd test root");
    println!("cwd growth bounded, kernel intact");
}
