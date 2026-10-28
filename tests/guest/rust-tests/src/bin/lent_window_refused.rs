//! What the kernel says of a handle lent as a window, asked as a file server
//! asks it (`SharedMemory::lent`): a pipe end is no memory it allocated to a
//! region, a region twice `WINDOW_BYTES` long is refused by its length, and a
//! region of `WINDOW_BYTES` maps.

use toyos::fs::WINDOW_BYTES;
use toyos::shm::{Lent, SharedMemory};
use toyos_abi::syscall;
use toyos::OwnedHandle;

/// A region of `bytes`, as a peer lends it.
fn region(bytes: usize) -> OwnedHandle {
    let made = syscall::shm_create(bytes).expect("lent_window_refused: a region");
    // SAFETY: a fresh handle nothing else answers for.
    unsafe { OwnedHandle::from_raw(made) }
}

fn main() {
    assert!(SharedMemory::lent(region(WINDOW_BYTES), WINDOW_BYTES).is_ok(), "lent_window_refused: a region of WINDOW_BYTES was refused");
    let (read, _write) = toyos::pipe_pair().expect("lent_window_refused: a pipe");
    // SAFETY: the pipe end gives up the handle it owned.
    let pipe = SharedMemory::lent(unsafe { OwnedHandle::from_raw(read.into_raw()) }, WINDOW_BYTES).err();
    assert_eq!(pipe, Some(Lent::NotRam), "lent_window_refused: a pipe end lent as a window");
    let twice = SharedMemory::lent(region(2 * WINDOW_BYTES), WINDOW_BYTES).err();
    assert_eq!(
        twice,
        Some(Lent::Length { is: 2 * WINDOW_BYTES, bytes: WINDOW_BYTES }),
        "lent_window_refused: a region twice WINDOW_BYTES long lent as a window"
    );
    println!("lent_window_refused: a pipe end is no region, and a region twice the window's length is refused by it");
}
