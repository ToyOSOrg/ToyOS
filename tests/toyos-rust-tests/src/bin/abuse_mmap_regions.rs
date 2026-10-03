//! A process's regions are bounded at `MAX_REGIONS`: past it every way a
//! mapping enters the address space is refused by name, a replacement — which
//! adds no region — is not, and one region freed is room for one.

use toyos_abi::syscall::{munmap, MmapFlags, MmapProt, SyscallError, MAX_REGIONS, SYS_MMAP};
use SyscallError::ResourceExhausted;

const PAGE_2M: u64 = 2 * 1024 * 1024;

/// The process's own regions — its segments, stack, TLS block and heap — that
/// the fill below never gets.
const OWN_REGIONS: usize = 64;

/// `syscall::mmap` answers every refusal with a null, so the error is read here.
fn map(addr: u64, prot: MmapProt, flags: MmapFlags) -> Result<u64, SyscallError> {
    let ret: u64;
    // SAFETY: a register-to-register `syscall`; no argument here is a pointer
    // this call dereferences.
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rdi") SYS_MMAP,
            in("rsi") addr,
            in("rdx") PAGE_2M,
            in("r8") prot.0,
            in("r9") flags.0,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
        );
    }
    SyscallError::from_u64(ret).map_or(Ok(ret), Err)
}

fn unmap(addr: u64) {
    unsafe { munmap(addr as *mut u8, PAGE_2M as usize) }
        .unwrap_or_else(|e| panic!("unmapping {addr:#x} was refused: {e:?}"));
}

fn main() {
    let anywhere = MmapFlags::ANONYMOUS | MmapFlags::PRIVATE;
    let placed = anywhere | MmapFlags::FIXED;
    let rw = MmapProt::READ | MmapProt::WRITE;

    let mut count = 0usize;
    let mut kept = [0u64; 2];
    let mut lowest = u64::MAX;
    let mut refusal = None;
    for _ in 0..=MAX_REGIONS {
        match map(0, MmapProt::NONE, anywhere) {
            Ok(addr) => {
                if let Some(slot) = kept.get_mut(count) {
                    *slot = addr;
                }
                lowest = lowest.min(addr);
                count += 1;
            }
            Err(e) => {
                refusal = Some(e);
                break;
            }
        }
    }
    let refusal = refusal.unwrap_or_else(|| {
        panic!("{count} PROT_NONE mappings were all placed: the region count is unbounded")
    });
    assert_eq!(refusal, ResourceExhausted, "the mapping after {count} was refused for another reason");
    assert!(
        count > MAX_REGIONS - OWN_REGIONS,
        "refused after {count} mappings, short of {MAX_REGIONS} by more than this process's own regions",
    );

    // Placement runs top-down, so nothing is registered below the fill.
    let free = lowest - 2 * PAGE_2M;
    assert_eq!(
        map(free, MmapProt::NONE, placed),
        Err(ResourceExhausted),
        "a placed mapping at a free address was not refused at the bound",
    );
    assert_eq!(map(0, rw, anywhere), Err(ResourceExhausted), "an RW mapping was not refused at the bound");
    assert_eq!(
        map(kept[0], rw, placed),
        Ok(kept[0]),
        "a placed mapping over one of this process's own was refused at the bound, though it adds no region",
    );

    unmap(kept[1]);
    assert_eq!(map(free, MmapProt::NONE, placed), Ok(free), "the placed mapping found no room a free region made");
    assert_eq!(map(0, rw, anywhere), Err(ResourceExhausted), "an RW mapping was placed past the bound");
    unmap(free);
    let served = map(0, rw, anywhere).expect("an RW mapping found no room a free region made");
    unmap(served);

    println!("{count} mappings placed and the next refused; at the bound a replacement maps, and a free region is room for one");
}
