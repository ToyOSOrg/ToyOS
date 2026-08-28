//! `usbd`: the kernel thread reserved for the xHCI port machine; the body only parks.

use toyos_sched::task::WaitClass;

use crate::completion::{self, Subject, Token};
use crate::sched::kthread::{self, OnPanic};
use crate::scheduler;
use crate::time::Deadline;

/// The name `sched::dump`, `ps` and a crash report use.
const NAME: &str = "usbd";

/// Start the thread. Called once, from `kernel_main`, beside `klogd`'s.
pub fn start() {
    // Unconditional: gating this on a controller would give the kernel's thread count a second answer.
    let _ = kthread::spawn(NAME, body, 0, OnPanic::Recover);
}

extern "C" fn body(_arg: u64) -> ! {
    // Must stay first: `actuator.rs` promises this panics on usbd's first instruction.
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::usbd_panic() {
        panic!("usbd-panic: the device thread died");
    }

    let parkable = scheduler::Parkable::at_entry();
    let handle = crate::sched::driver::current_handle().expect("usbd runs as a task");
    // Armed once for the loop: rearming here would drop a pending wake.
    let armed = completion::arm(
        Subject::of(handle.watch()),
        Token::new(0),
        WaitClass::Io,
    )
    .expect("a kernel thread is a task and can arm");
    loop {
        // No deadline: only an interrupt or a port's own deadline should wake this.
        // The cancel arm never fires: nothing retires a kernel thread.
        let _ = completion::wait(&parkable, &armed, Deadline::never());
    }
}
