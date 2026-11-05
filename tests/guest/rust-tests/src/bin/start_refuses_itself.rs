//! A real std program whose dynamic section names what its own start refuses
//! exits [`REFUSED_STATUS`] and says why on the stderr it is endowed, and says
//! nothing and exits the same endowed none: a `RELR` table, which
//! `toyos::relocate` refuses before it writes a word, and a `DT_NEEDED` naming
//! no library, which `toyos::dl::start` refuses before `main`. The kernel
//! parses neither; the parent lays the copy out and the copy refuses itself.

#[path = "../image.rs"]
mod image;

use std::fs;
use std::process::Command;

use toyos_abi::end::End;
use toyos_abi::syscall::{self, SpawnArgs, SyscallError};
use toyos_abi::RawHandle;

const DIR: &str = "/tmp/start_refuses_itself";

/// What `toyos::relocate` writes to its stderr, `fd:2`, when it refuses its own program.
const REFUSED_LINE: &str = "this program's relocations are refused: it cannot start\n";
/// What the start writes there for a program whose `DT_NEEDED` names no library.
const NO_LIBRARY_LINE: &str = "this program's libraries could not be loaded: ELF: a DT_NEEDED names no library\n";
/// The status it exits with then.
const REFUSED_STATUS: i32 = 127;
/// The argument a refused copy is handed, which its `main` panics on.
const REFUSED_COPY: &str = "--refused-copy";

const PT_DYNAMIC: u32 = 2;
const PH_SIZE: usize = 56;
const DT_NEEDED: u64 = 1;
const DT_DEBUG: u64 = 21;
const DT_RELR: u64 = 36;

/// This program's own file with its `DT_DEBUG` entry's tag made `tag`: a std
/// image as lld linked it, whose dynamic section now names `tag` ahead of its
/// `DT_RELA`. `DT_NEEDED`'s value, 0, names the empty string.
fn own_image_tagged(tag: u64) -> Vec<u8> {
    let exe = std::env::current_exe().expect("current_exe");
    let mut bytes = fs::read(&exe).unwrap_or_else(|e| panic!("read {exe:?}: {e}"));
    let word = |bytes: &[u8], at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    let phoff = word(&bytes, 32) as usize;
    let phnum = usize::from(u16::from_le_bytes([bytes[56], bytes[57]]));
    let dynamic = (0..phnum)
        .map(|i| phoff + i * PH_SIZE)
        .find(|&at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) == PT_DYNAMIC)
        .expect("lld's PT_DYNAMIC");
    let (offset, size) = (word(&bytes, dynamic + 8) as usize, word(&bytes, dynamic + 32) as usize);
    let entry = (offset..offset + size).step_by(16).find(|&at| word(&bytes, at) == DT_DEBUG).expect("lld's DT_DEBUG");
    bytes[entry..entry + 8].copy_from_slice(&tag.to_le_bytes());
    bytes
}

/// `path` read into an image and spawned with `argv`, NUL-separated, endowed nothing.
fn spawn_endowed_nothing(path: &str, argv: &str) -> Result<RawHandle, SyscallError> {
    let image = image::image_of(path);
    let laid = image::laid(&image);
    // SAFETY: every pointer names a live local for the whole call.
    unsafe {
        syscall::spawn(&SpawnArgs {
            path_ptr: path.as_ptr() as u64,
            path_len: path.len() as u64,
            argv_ptr: argv.as_ptr() as u64,
            argv_len: argv.len() as u64,
            env_ptr: 0,
            env_len: 0,
            endow_ptr: 0,
            endow_count: 0,
            labels_ptr: 0,
            labels_len: 0,
            maps_ptr: laid.maps().as_ptr() as u64,
            maps_count: laid.maps().len() as u64,
            entry: laid.entry,
            image_base: laid.image_base,
            build_id_ptr: laid.build_id().as_ptr() as u64,
            build_id_len: laid.build_id().len() as u64,
            place: u64::from(toyos_abi::HANDLE_INVALID.0),
            pager: u64::from(toyos_abi::HANDLE_INVALID.0),
        })
    }
}

fn refuses_itself(name: &str, tag: u64, line: &str) {
    let path = format!("{DIR}/{name}");
    fs::write(&path, own_image_tagged(tag)).unwrap_or_else(|e| panic!("write {path}: {e}"));
    let out = Command::new(&path).arg(REFUSED_COPY).output().unwrap_or_else(|e| panic!("{name}: spawn: {e}"));
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(REFUSED_STATUS), "{name}: exited {:?}, said {said:?}", out.status);
    assert_eq!(said, line, "{name}");

    let child = spawn_endowed_nothing(&path, &format!("{path}\0{REFUSED_COPY}\0"))
        .unwrap_or_else(|e| panic!("{name} without a stderr: spawn gave {e:?}"));
    // SAFETY: the handle `SYS_SPAWN` just answered, held nowhere else.
    let child = unsafe { toyos::process::Process::from_raw(child) };
    let end = child.wait().unwrap_or_else(|e| panic!("{name} without a stderr: wait: {e:?}"));
    assert_eq!(end, End::Exited(REFUSED_STATUS), "{name} without a stderr");
    println!("start_refuses_itself: {name}: refused with 127, and said why where it had a stderr");
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some(REFUSED_COPY) {
        panic!("a copy whose start must refuse it reached main");
    }
    fs::create_dir_all(DIR).expect("create the test's directory");
    refuses_itself("carries_relr", DT_RELR, REFUSED_LINE);
    refuses_itself("needs_a_library", DT_NEEDED, NO_LIBRARY_LINE);
    let _ = fs::remove_dir_all(DIR);
    println!("start_refuses_itself: PASS");
}
