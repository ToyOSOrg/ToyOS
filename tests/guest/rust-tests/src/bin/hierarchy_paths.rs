//! Synthetic directories expose only ancestors of endowed filesystem capabilities.

use std::fs;
use std::os::toyos::process::CommandExt;
use std::process::Command;

use toyos::files::{self, At, Open};
use toyos::endow::{self, service_label, SVC_PREFIX};
use toyos::port;
use toyos_abi::syscall::FileType;

const SELF: &str = "/system/bin/test_rs_hierarchy_paths";
const PROBE: &str = "/tmp/hierarchy_paths_probe";

fn names(dir: &str) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {dir}: {e}"))
        .map(|e| e.expect("dir entry").file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// The services this process holds, sorted.
fn services() -> Vec<&'static str> {
    let mut held: Vec<_> = endow::services().collect();
    held.sort();
    held
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("empty") => {
            assert!(services().is_empty());
            assert!(fs::metadata("/").expect("empty root").is_dir());
            assert!(names("/").is_empty());
            std::env::set_current_dir("/").expect("cwd in empty root");
            assert!(fs::read(PROBE).is_err(), "a process given no service reached /tmp");
        }
        Some("narrow") => narrowed(),
        Some(other) => panic!("unknown role {other}"),
        None => {
            fs::write(PROBE, b"endowed").expect("create probe");
            // The unserved connector supplies an ancestor, without granting any
            // global filesystem access or requiring a filesystem implementation.
            let (_acceptor, connector) = port::create().expect("port");
            let narrow = Command::new(SELF)
                .arg("narrow")
                .services_only(&["fs:/tmp"])
                .endow(&service_label(SVC_PREFIX, "fs:/nested/leaf"), connector.into())
                .status();
            assert!(narrow.expect("spawn the narrowed probe").success(), "narrow child failed");
            let empty = Command::new(SELF).arg("empty").services_only(&[]).status();
            assert!(empty.expect("spawn the empty probe").success(), "empty child failed");
            fs::remove_file(PROBE).expect("remove probe");
            println!("all hierarchy tests passed");
        }
    }
}

fn narrowed() {
    assert_eq!(services(), ["fs:/nested/leaf", "fs:/tmp"]);
    assert_eq!(names("/"), ["nested", "tmp"]);
    assert_eq!(names("/nested"), ["leaf"]);
    assert!(fs::metadata("/nested").expect("synthetic ancestor").is_dir());
    assert!(fs::metadata("/system").is_err(), "an unendowed root name appeared");

    let root = files::open(At::Cwd, b"/", Open { read: true, directory: true, ..Open::default() }).expect("open root");
    assert_eq!(files::fstat(root).expect("root fstat").file_type, FileType::Directory);
    let file = files::open(At::Fd(root), b"tmp/hierarchy_paths_probe", Open { read: true, ..Open::default() }).expect("open from synthetic root");
    let mut bytes = [0; 7];
    assert_eq!(files::read(file, &mut bytes).expect("read relative file"), bytes.len());
    assert_eq!(&bytes, b"endowed");
    // SAFETY: these descriptors were opened here and are held nowhere else.
    unsafe { toyos::fd::close(file).unwrap(); toyos::fd::close(root).unwrap(); }

    assert!(fs::write("/intruder", b"x").is_err());
    assert!(fs::remove_file("/nested").is_err());
    assert!(fs::rename("/nested", "/elsewhere").is_err());
    assert!(fs::create_dir("/intruder").is_err());
    assert!(fs::remove_dir("/").is_err());
    assert!(std::os::toyos::fs::symlink("/tmp", "/intruder").is_err());
    assert_eq!(names("/"), ["nested", "tmp"], "a refused mutation changed the root");
}
