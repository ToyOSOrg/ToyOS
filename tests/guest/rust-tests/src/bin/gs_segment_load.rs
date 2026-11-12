//! Ring 3 loads GS with each selector it may and enters the kernel, which
//! keeps its own GS base.
//!
//! A segment load writes the descriptor's base along with the selector (SDM
//! Vol. 2B, MOV), and a kernel that addressed its per-CPU block through
//! whatever GS held would store through the user's base, 0 here, on the next
//! entry. The child loads every selector this GDT lets CPL 3 load — the null
//! ones and the user data and code descriptors, at each RPL — and then enters
//! the kernel three ways: `SYSCALL` (a `getpid`), a Ring 3 page fault on a
//! page not yet populated, and a sleep that parks it. After each it reads a
//! marker through GS, which finds the marker only where the base it loaded is
//! still its own. Every other selector raises `#GP` at CPL 3.

use std::os::toyos::process::ExitStatusExt;
use std::process::Command;

use toyos_abi::end::End;
use toyos_abi::syscall::{self, VmAt, VmProt};

#[path = "../arch/gs.rs"]
mod gs;

const SELF_PATH: &str = "/system/bin/test_rs_gs_segment_load";

/// Every selector CPL 3 may load into GS: null, user data (0x18) and user
/// code (0x20), each at RPL 0 to 3. The code descriptor is readable, so a data
/// register takes it.
const SELECTORS: [u16; 12] = [0x00, 0x01, 0x02, 0x03, 0x18, 0x19, 0x1a, 0x1b, 0x20, 0x21, 0x22, 0x23];

/// What a read through GS at this static's address finds when GS's base is 0.
static MARKER: u64 = 0x6773_2062_6173_6520;

const PAGE: usize = 4096;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("child") => child(),
        Some(other) => panic!("unknown role {other:?}"),
        None => parent(),
    }
}

fn parent() {
    let out = Command::new(SELF_PATH).arg("child").output().expect("spawn the child");
    let said = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        End::from_word(out.status.into_raw()),
        Some(End::Exited(0)),
        "the child ended {} having said:\n{said}",
        out.status,
    );
    for selector in SELECTORS {
        let line = format!("gs {selector:#04x}: ");
        assert!(said.lines().any(|l| l.starts_with(&line)), "the child never reported selector {selector:#04x}:\n{said}");
    }
    print!("{said}");
    println!("gs_segment_load: {} selectors loaded into GS, each through a syscall, a page fault and a park", SELECTORS.len());
}

fn child() {
    let pid = std::process::id();
    let marker = &raw const MARKER as u64;
    // SAFETY: a fresh range nothing else names; committed, so each page is
    // populated by its first touch below, a Ring 3 page fault.
    let region = unsafe { syscall::vm_reserve(SELECTORS.len() * PAGE, VmProt::ReadWrite, VmAt::ANYWHERE) }
        .expect("reserve");
    for (i, selector) in SELECTORS.into_iter().enumerate() {
        gs::load_gs(selector);
        let answered = syscall::getpid().raw();
        let after_syscall = read(marker);
        // SAFETY: inside the range reserved above, a page no earlier write touched.
        unsafe { region.add(i * PAGE).write_volatile(1) };
        let after_fault = read(marker);
        syscall::nanosleep(1_000_000);
        let after_park = read(marker);
        assert_eq!(answered, pid, "getpid answered {answered} under selector {selector:#04x}");
        for (entry, word) in [("a syscall", after_syscall), ("a page fault", after_fault), ("a park", after_park)] {
            assert_eq!(word, MARKER, "after {entry} under selector {selector:#04x}, GS read {word:#x}");
        }
        println!("gs {selector:#04x}: getpid answered {answered}, and GS's base was 0 after a syscall, a page fault and a park");
    }
    // SAFETY: nothing reaches the range after this.
    unsafe { syscall::vm_release(region, SELECTORS.len() * PAGE) }.expect("release");
}

/// The marker's word as GS addresses it.
fn read(marker: u64) -> u64 {
    // SAFETY: every selector this loads has base 0 (or keeps one, null on
    // AMD, that was 0), so this is the marker's own eight bytes.
    unsafe { gs::read_through_gs(marker) }
}
