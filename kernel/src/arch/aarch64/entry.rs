//! Where a new context starts: the frame `switch` restores and the
//! trampolines to user mode and to a kernel thread, the port's stages 4 and 7.


pub(crate) extern "C" fn process_start() {
    owed!("user mode", "stage 7")
}

pub(crate) extern "C" fn thread_start() {
    owed!("user mode", "stage 7")
}

pub(crate) extern "C" fn kernel_start() {
    owed!("kernel threads", "stage 4")
}

/// # Safety
/// `top` is the end of a fresh kernel stack nothing else references.
pub unsafe fn initial_frame(
    _top: u64,
    _trampoline: unsafe extern "C" fn(),
    _user_entry: u64,
    _user_sp: u64,
    _arg: u64,
) -> u64 {
    owed!("kernel threads", "stage 4")
}
