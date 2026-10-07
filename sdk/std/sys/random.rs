pub fn fill_bytes(buf: &mut [u8]) {
    if let Err(e) = toyos_abi::syscall::random(buf) {
        panic!("failed to generate random data: {e:?}");
    }
}
