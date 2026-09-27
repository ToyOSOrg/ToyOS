use std::process::Command;

fn main() {
    test_user_segfault();
    test_system_alive();
    println!("all panic recovery tests passed");
}

/// User-mode segfault → process killed, system survives.
fn test_user_segfault() {
    let status = Command::new("/system/bin/test_rs_segfault_child")
        .status()
        .expect("failed to spawn child");
    assert!(!status.success(), "child that segfaults should be killed");
    println!("  PASS: user segfault killed process (exit={})", status.code().unwrap_or(-1));
}

/// System still works after the segfault.
fn test_system_alive() {
    let output = Command::new("/system/bin/echo")
        .arg("still alive")
        .output()
        .expect("failed to run echo after the segfault");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "still alive");
    println!("  PASS: system alive after a user segfault");
}
