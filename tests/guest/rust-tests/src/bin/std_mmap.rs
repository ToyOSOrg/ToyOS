use toyos_abi::syscall::{self, VmAt, VmProt};

fn main() {
    let size: usize = 2 * 1024 * 1024;
    let ptr = unsafe { syscall::vm_reserve(size, VmProt::ReadWrite, VmAt::ANYWHERE).unwrap_or(core::ptr::null_mut()) };
    assert!(!ptr.is_null(), "mmap failed");

    for i in 0..size {
        unsafe { ptr.add(i).write(0xAB) };
    }

    for i in 0..size {
        let val = unsafe { ptr.add(i).read() };
        assert_eq!(val, 0xAB, "mismatch at offset {i}: expected 0xAB, got {val:#x}");
    }

    unsafe { syscall::vm_release(ptr, size) }.expect("munmap failed");

    println!("all mmap tests passed");
}
