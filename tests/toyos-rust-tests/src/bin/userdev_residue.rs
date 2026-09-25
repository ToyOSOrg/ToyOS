//! Two claims of one function nothing resets, the first closed while its grant
//! stays mapped: the second holder's memory is its own.
//!
//! **A claim is an ordinary handle, and a grant outlives it.** The first role
//! claims QEMU's 82574 through the test estate's capability, grants one buffer
//! and fills it, then closes the claim and keeps the buffer mapped. The boot
//! arms `pcidev-reset-nothing`, so that release keeps the addresses the function
//! was left aimed at for its next claim. A child then claims the same function,
//! is granted a buffer of the same size at the same device address, finds it
//! zeroed, and fills it with its own word.
//!
//! The verdict is the first role's own buffer after the child exits: every word
//! still the first holder's. The child's word there is two processes sharing
//! live DMA memory; a zero is the kernel clearing pages under a live mapping.
//!
//! Roles: no argument is the first holder; `second` is the child.

use std::io::Read;
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};

use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos::PciDev;
use toyos_abi::syscall::{PciId, SYSCAP_LABEL};

/// The 82574, QEMU's `e1000e`: a function this kernel does not drive, and which
/// the test estate's boot hands to nobody else.
const E82574: PciId = PciId { vendor: 0x8086, device: 0x10d3 };

/// One 2 MiB leaf, the size of every grant the second claim is matched on.
const BYTES: u64 = 2 * 1024 * 1024;

const FIRST_WORD: u64 = 0x1111_1111_f1f1_f1f1;
const SECOND_WORD: u64 = 0x2222_2222_f2f2_f2f2;

const SELF_PATH: &str = "/system/bin/test_rs_userdev_residue";

/// Where the child says what it was granted.
const PLACED: &str = "placed at ";

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("userdev_residue: the test estate is endowed a device-minting capability");
    match std::env::args().nth(1).as_deref() {
        Some("second") => second(&cap),
        Some(other) => panic!("userdev_residue: unknown role {other:?}"),
        None => first(&cap),
    }
}

fn words(memory: &toyos::shm::SharedMemory) -> impl Iterator<Item = *mut u64> + '_ {
    let base = memory.as_ptr() as *mut u64;
    // SAFETY: every offset is inside the mapped grant, which is `len` bytes.
    (0..memory.len() / 8).map(move |index| unsafe { base.add(index) })
}

fn fill(memory: &toyos::shm::SharedMemory, word: u64) {
    // SAFETY: each pointer is an aligned word of this process's mapping.
    words(memory).for_each(|at| unsafe { at.write_volatile(word) });
}

/// The first word that is not `word`, as `(index, value)`.
fn stray(memory: &toyos::shm::SharedMemory, word: u64) -> Option<(usize, u64)> {
    // SAFETY: as `fill`.
    words(memory)
        .map(|at| unsafe { at.read_volatile() })
        .enumerate()
        .find(|(_, value)| *value != word)
}

fn first(cap: &SysCap) {
    let dev: PciDev = cap.claim_pci(E82574).expect("userdev_residue: the first claim of the 82574");
    let grant = dev.dma_alloc(BYTES).expect("userdev_residue: the first claim's grant");
    fill(&grant.memory, FIRST_WORD);
    // The claim closes here; the grant's mapping is this process's and stays.
    drop(dev);

    let dup = cap.duplicate().expect("userdev_residue: a capability for the second holder");
    let mut command = Command::new(SELF_PATH);
    command.arg("second").endow(SYSCAP_LABEL, dup.into_raw().0);
    let mut child = command.stdout(Stdio::piped()).spawn().expect("userdev_residue: spawn the second holder");
    let mut said = String::new();
    child
        .stdout
        .take()
        .expect("userdev_residue: the second holder's stdout")
        .read_to_string(&mut said)
        .expect("userdev_residue: read the second holder");
    let status = child.wait().expect("userdev_residue: reap the second holder");
    assert!(status.success(), "userdev_residue: the second holder failed ({status:?}): {said}");
    let placed = said
        .trim()
        .strip_prefix(PLACED)
        .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
        .unwrap_or_else(|| panic!("userdev_residue: the second holder said {said:?}"));
    assert_eq!(
        placed, grant.device_addr,
        "userdev_residue: the premise: the second claim's grant is not where the first claim's was"
    );

    match stray(&grant.memory, FIRST_WORD) {
        None => {}
        Some((index, SECOND_WORD)) => panic!(
            "userdev_residue: word {index} of the first holder's grant is the second holder's: two \
             claims share one grant's pages"
        ),
        Some((index, value)) => panic!(
            "userdev_residue: word {index} of the first holder's grant is {value:#x} under its live \
             mapping, neither holder's word"
        ),
    }
    println!(
        "userdev_residue: the second claim was placed at {placed:#x}, the first claim's address, and \
         the first holder's grant is still its own"
    );
}

fn second(cap: &SysCap) {
    let dev: PciDev = cap.claim_pci(E82574).expect("userdev_residue: the second claim of the 82574");
    let grant = dev.dma_alloc(BYTES).expect("userdev_residue: the second claim's grant");
    if let Some((index, value)) = stray(&grant.memory, 0) {
        panic!("userdev_residue: word {index} of the second claim's first grant reads {value:#x}, not zero");
    }
    fill(&grant.memory, SECOND_WORD);
    println!("{PLACED}{:#x}", grant.device_addr);
}
