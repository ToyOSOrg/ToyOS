//! The GS-base primitive is `#UD` at Ring 3 here. Its probe is a child, so the
//! #UD kills the child and this parent plus `echo` outlive a Ring 3 -> 0 write.

use std::process::Command;

fn main() {
    let status = Command::new("/system/bin/test_rs_gsbase_probe").status().expect("spawn gsbase_probe");
    if status.success() {
        println!("FAIL the gsbase primitive is present at ring 3 (exit {:?})", status.code());
        std::process::exit(1);
    }
    let out = Command::new("/system/bin/echo").arg("still alive").output().expect("spawn echo");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "still alive",
        "the machine survived the probe but can no longer start a process",
    );
    println!("PASS rdgsbase/wrgsbase are #UD at ring 3; per-CPU state intact");
}
