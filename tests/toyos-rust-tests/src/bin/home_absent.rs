//! On a boot where the DATA volume is ours and did not mount, `/apps`,
//! `/config`, `/home` and `/state` must never come back as a place to write:
//! the kernel's own log line and the byte-identical image (asserted on the
//! host, in `tests/common/storage.rs`) do not observe that from inside the
//! guest — this does. Driven by `broken_data_volume_is_absent` and
//! `data_candidate_with_bad_geometry_is_absent` alone: every other boot mounts
//! the four, on the DATA volume or on a tmpfs, and every check below would
//! fail on it.

use std::io::ErrorKind;

fn main() {
    let mut wrong = Vec::new();

    for dir in ["/apps", "/config", "/home", "/state"] {
        match std::fs::write(format!("{dir}/x"), b"should never land") {
            Err(e) if e.kind() == ErrorKind::PermissionDenied => {}
            other => wrong.push(format!("writing {dir}/x: {other:?}, want PermissionDenied")),
        }
    }
    match std::env::set_current_dir("/home/toy") {
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        other => wrong.push(format!("chdir /home/toy: {other:?}, want NotFound")),
    }
    match std::fs::read_dir("/home") {
        Ok(entries) => {
            let names: Vec<_> = entries.map(|e| e.expect("a readdir entry").file_name()).collect();
            if !names.is_empty() {
                wrong.push(format!("/home lists {names:?}, want empty"));
            }
        }
        Err(e) => wrong.push(format!("listing /home: {e:?}, want Ok(empty)")),
    }

    assert!(wrong.is_empty(), "an absent DATA volume was not absent:\n{}", wrong.join("\n"));
    println!(
        "home-absent: /apps, /config, /home and /state refused every write, and /home/toy every \
         chdir and listing"
    );
}
