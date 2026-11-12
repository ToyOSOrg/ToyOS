//! Once a program has relocated itself its `PT_GNU_RELRO` pages are
//! read-only: a copy of this program that stores to a word its relocations
//! wrote is ended as a memory fault, and never reaches the line after the
//! store.

use std::os::toyos::process::ExitStatusExt;
use std::process::Command;

use toyos_abi::end::{End, Fault};

const STORES: &str = "stores";
/// What the storing copy exits with when the word is in no whole page of its
/// `PT_GNU_RELRO`: the test's premise is gone, not its subject.
const NOT_IN_RELRO: i32 = 3;

const PT_LOAD: u32 = 1;
const PT_GNU_RELRO: u32 = 0x6474_e552;
const PH_SIZE: usize = 56;
const PAGE: usize = 4096;

fn answer() -> u32 {
    42
}

/// A function's address, which the linker cannot know: a word this program's
/// relocations write, and so one the linker puts in `PT_GNU_RELRO`.
static TABLE: [fn() -> u32; 1] = [answer];

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some(STORES) => stores(),
        Some(other) => panic!("relro_sealed: unknown role {other:?}"),
        None => holder(),
    }
}

/// The whole pages of this program's `PT_GNU_RELRO`, as addresses.
fn relro_pages() -> Option<(usize, usize)> {
    unsafe extern "C" {
        static __ehdr_start: u8;
    }
    let header = &raw const __ehdr_start;
    // SAFETY: the image's own header and program header table, which its
    // first segment maps.
    unsafe {
        let phoff = header.add(32).cast::<u64>().read_unaligned() as usize;
        let phnum = usize::from(header.add(56).cast::<u16>().read_unaligned());
        let kind = |i: usize| header.add(phoff + i * PH_SIZE).cast::<u32>().read_unaligned();
        let word = |i: usize, at: usize| header.add(phoff + i * PH_SIZE + at).cast::<u64>().read_unaligned() as usize;
        // The header is the first byte of the `PT_LOAD` at file offset 0.
        let first = (0..phnum).find(|&i| kind(i) == PT_LOAD && word(i, 8) == 0)?;
        let relro = (0..phnum).find(|&i| kind(i) == PT_GNU_RELRO)?;
        let start = header.addr() + word(relro, 16) - word(first, 16);
        Some((start & !(PAGE - 1), (start + word(relro, 40)) & !(PAGE - 1)))
    }
}

fn stores() {
    let slot = std::hint::black_box((&raw const TABLE).cast_mut().cast::<usize>());
    match relro_pages() {
        Some((lo, hi)) if (lo..hi).contains(&slot.addr()) => {}
        pages => {
            eprintln!("relro_sealed: the table at {:#x} is in no whole page of PT_GNU_RELRO ({pages:x?})", slot.addr());
            std::process::exit(NOT_IN_RELRO);
        }
    }
    // SAFETY: none: the store is the test, and the kernel ends this program at it.
    unsafe { slot.write_volatile(0) };
    println!("relro_sealed: the store to PT_GNU_RELRO landed");
}

fn holder() {
    assert_eq!(TABLE[0](), 42, "relro_sealed: the relocated word does not read");
    let exe = std::env::current_exe().expect("relro_sealed: current_exe");
    let status = Command::new(&exe).arg(STORES).status().expect("relro_sealed: its storing copy");
    assert_eq!(
        End::from_word(status.into_raw()),
        Some(End::Fault(Fault::Memory)),
        "relro_sealed: a store to PT_GNU_RELRO did not end the program as a memory fault ({status})",
    );
    println!("relro_sealed: a store to a relocated word of PT_GNU_RELRO ended its program as a memory fault");
}
