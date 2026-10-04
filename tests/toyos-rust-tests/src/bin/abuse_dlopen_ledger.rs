//! A process's libraries are bounded at `MAX_LIBRARIES`: past it a load of a
//! name it does not hold is refused by name having registered nothing, and a
//! name it holds still answers.

use toyos_abi::syscall::{self, SyscallError, MAX_LIBRARIES};

const DIR: &str = "/tmp/dlopen-ledger";

/// Loads attempted: past where a library ledger with no bound outgrows one
/// heap allocation.
const ATTEMPTS: usize = 4 * MAX_LIBRARIES;

/// One `PT_LOAD`, read-only and exactly 2 MiB long, so the image has no
/// writable window and the shared-object cache keeps none of it: every load is
/// its own, and nothing of this test outlives it.
fn image() -> Vec<u8> {
    const LEN: usize = 0x1000;
    const SPAN: u64 = 2 * 1024 * 1024;
    let mut elf = vec![0u8; LEN];
    elf[..8].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0]);
    elf[16..18].copy_from_slice(&3u16.to_le_bytes()); // ET_DYN
    elf[18..20].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
    elf[20..24].copy_from_slice(&1u32.to_le_bytes()); // e_version
    elf[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
    elf[52..54].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
    elf[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
    elf[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
    let ph = &mut elf[64..120];
    ph[0..4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
    ph[4..8].copy_from_slice(&5u32.to_le_bytes()); // PF_R | PF_X
    ph[32..40].copy_from_slice(&(LEN as u64).to_le_bytes()); // p_filesz
    ph[40..48].copy_from_slice(&SPAN.to_le_bytes()); // p_memsz
    ph[48..56].copy_from_slice(&0x1000u64.to_le_bytes()); // p_align
    elf
}

/// The libraries this process holds, the executable not among them.
fn libraries() -> usize {
    let need = syscall::query_modules(&mut []).expect("the modules' size");
    let mut answer = vec![0u8; need];
    let got = syscall::query_modules(&mut answer).expect("the modules");
    syscall::modules(&answer[..got]).count() - 1
}

fn main() {
    std::fs::create_dir_all(DIR).unwrap_or_else(|e| panic!("make {DIR}: {e}"));
    let lib = format!("{DIR}/lib.so");
    std::fs::write(&lib, image()).unwrap_or_else(|e| panic!("write {lib}: {e}"));
    let held = libraries();

    let mut first = None;
    let mut loaded = 0usize;
    let mut refusal = None;
    for i in 0..ATTEMPTS {
        // A name of its own for one file: the ledger is keyed by name.
        let name = format!("{DIR}/{i}.so");
        syscall::symlink(lib.as_bytes(), name.as_bytes()).unwrap_or_else(|e| panic!("link {name}: {e:?}"));
        match syscall::dl_open(name.as_bytes()) {
            Ok(handle) => {
                first.get_or_insert((name, handle));
                loaded += 1;
            }
            Err(e) => {
                refusal = Some(e);
                break;
            }
        }
    }
    let refusal = refusal.unwrap_or_else(|| {
        panic!("{loaded} names of one library all loaded: the library count is unbounded")
    });
    assert_eq!(
        refusal,
        SyscallError::ResourceExhausted,
        "the load after {loaded} was refused for another reason",
    );
    assert_eq!(held + loaded, MAX_LIBRARIES, "refused after {loaded} loads beside the {held} it started with");
    assert_eq!(libraries(), MAX_LIBRARIES, "the refused load is registered");

    let (name, handle) = first.expect("a library was loaded");
    assert_eq!(syscall::dl_open(name.as_bytes()), Ok(handle), "a name already held was refused at the bound");

    println!("{loaded} libraries loaded and the next name refused; a name held still answers");
}
