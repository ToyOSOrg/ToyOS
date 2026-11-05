//! The kernel must survive an ELF whose program headers lie about their sizes
//! or place a segment outside the loaded image.
//!
//! `p_filesz` and `p_memsz` are a (copy length, buffer size) pair to a loader,
//! so the spec's `p_filesz <= p_memsz` is memory safety: inverted, a copy
//! overruns its destination with file-chosen length *and* content. Every case
//! must be an error return, with the kernel intact afterwards. A library's
//! headers are the program's own loader's to refuse (`toyos::dl`).

use std::fs;

use toyos_abi::syscall::{self, SpawnArgs, SyscallError};


/// Kernel-served, so a path spawn reaches the kernel's own open; every refusal
/// is asked again with the same bytes handed over as an image.
const DIR: &str = "/tmp/abuse_elf";

const ET_DYN: u16 = 3;
const EM_X86_64: u16 = 62;
const PT_LOAD: u32 = 1;
const PT_TLS: u32 = 7;

/// One PT_LOAD covering the whole file plus one extra header the caller fills.
/// `entry` and the load segment are honest; only the extra header lies.
fn elf(extra: Phdr) -> Vec<u8> {
    let mut out = vec![0u8; 64 + 2 * 56];

    out[..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
    out[4] = 2; // ELFCLASS64
    out[5] = 1; // ELFDATA2LSB
    out[6] = 1; // EV_CURRENT
    out[16..18].copy_from_slice(&ET_DYN.to_le_bytes());
    out[18..20].copy_from_slice(&EM_X86_64.to_le_bytes());
    out[20..24].copy_from_slice(&1u32.to_le_bytes());
    out[24..32].copy_from_slice(&0x1000u64.to_le_bytes()); // e_entry
    out[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
    out[52..54].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
    out[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
    out[56..58].copy_from_slice(&2u16.to_le_bytes()); // e_phnum
    out[58..60].copy_from_slice(&64u16.to_le_bytes()); // e_shentsize

    let load = Phdr { kind: PT_LOAD, flags: 5, offset: 0, vaddr: 0, filesz: 0x2000, memsz: 0x2000, align: 0x1000 };
    load.write(&mut out[64..120]);
    extra.write(&mut out[120..176]);

    // Give the file real content past the headers, so an overrunning copy
    // moves a recognizable pattern rather than zeros.
    out.resize(0x4000, 0x41);
    out
}

struct Phdr {
    kind: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
    align: u64,
}

impl Phdr {
    fn write(&self, out: &mut [u8]) {
        out[0..4].copy_from_slice(&self.kind.to_le_bytes());
        out[4..8].copy_from_slice(&self.flags.to_le_bytes());
        out[8..16].copy_from_slice(&self.offset.to_le_bytes());
        out[16..24].copy_from_slice(&self.vaddr.to_le_bytes());
        out[24..32].copy_from_slice(&self.vaddr.to_le_bytes()); // p_paddr
        out[32..40].copy_from_slice(&self.filesz.to_le_bytes());
        out[40..48].copy_from_slice(&self.memsz.to_le_bytes());
        out[48..56].copy_from_slice(&self.align.to_le_bytes());
    }
}

/// Write `bytes` to `path` and try to spawn it, then spawn the same bytes as an
/// image. Returns the spawn error, which the two routes must agree on.
fn spawn_err(name: &str, bytes: &[u8]) -> SyscallError {
    let path = format!("{DIR}/{name}");
    fs::write(&path, bytes).unwrap_or_else(|e| panic!("write {path}: {e}"));
    let by_path = spawn_as(&path, &[]);
    let by_image = spawn_as(&path, bytes);
    assert_eq!(by_path, by_image, "{name}: the path and the image were refused differently");
    by_path
}

/// A raw spawn of `path`, handing `image` over in a memory object of this
/// process's own when it is not empty.
fn spawn_as(path: &str, image: &[u8]) -> SyscallError {
    let argv = format!("{path}\0");
    let object = toyos::shm::SharedMemory::create(image.len().max(1)).expect("a memory object for the image");
    object.bytes().write(0, image);
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
            image: toyos::AsHandle::as_handle(&object).raw().0 as u64,
            image_len: image.len() as u64,
            place: u64::from(toyos_abi::HANDLE_INVALID.0),
            pager: u64::from(toyos_abi::HANDLE_INVALID.0),
        })
    }
    .map(|pid| panic!("{path}: spawn succeeded (pid {pid:?}) — the header is malformed"))
    .unwrap_err()
}

fn main() {
    fs::create_dir_all(DIR).expect("create /tmp/abuse_elf");

    // 1. PT_TLS filesz > memsz, which the program-header parse refuses.
    let err = spawn_err("tls_filesz", &elf(Phdr {
        kind: PT_TLS, flags: 4, offset: 0x2000, vaddr: 0x2000,
        filesz: 0x4000, memsz: 16, align: 8,
    }));
    assert_eq!(err, SyscallError::InvalidArgument, "PT_TLS filesz > memsz");

    // 3. PT_TLS vaddr far outside every PT_LOAD.
    let err = spawn_err("tls_vaddr", &elf(Phdr {
        kind: PT_TLS, flags: 4, offset: 0x2000, vaddr: 0x8000_0000,
        filesz: 8, memsz: 16, align: 8,
    }));
    assert_eq!(err, SyscallError::InvalidArgument, "PT_TLS vaddr outside the image");

    // 4. A second PT_LOAD with filesz > memsz. On the exe path the region
    //    bookkeeping clamps this, but it must still be refused rather than
    //    reaching the loader with an impossible header.
    let err = spawn_err("load_filesz", &elf(Phdr {
        kind: PT_LOAD, flags: 6, offset: 0x2000, vaddr: 0x4000,
        filesz: 0x2000, memsz: 0x10, align: 0x1000,
    }));
    assert_eq!(err, SyscallError::InvalidArgument, "PT_LOAD filesz > memsz");

    // The kernel heap is intact: allocate and touch enough to walk it, then
    // prove spawn still works.
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    for i in 0..256 {
        blocks.push(vec![(i % 251) as u8; 4096]);
    }
    for (i, b) in blocks.iter().enumerate() {
        assert!(b.iter().all(|&x| x == (i % 251) as u8), "kernel heap corrupted block {i}");
    }

    let status = std::process::Command::new("/system/bin/echo")
        .arg("loader still works")
        .status()
        .expect("spawn /system/bin/echo");
    assert!(status.success(), "/system/bin/echo exited {status:?}");

    let _ = fs::remove_dir_all(DIR);
    println!("malformed ELF program headers rejected, kernel intact");
}
