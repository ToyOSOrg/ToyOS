//! The relocator's oracle fixture: every word lld relocates in a static PIE
//! of it, a table of pointers, a table of vtables and a table of functions,
//! and with `--cfg tls` a thread-local holding a pointer, which lld relocates
//! inside the `PT_TLS` template. `toyos/src/relocate/tests.rs` says how each
//! file beside this one is made.

#![no_std]
#![cfg_attr(tls, feature(thread_local))]

pub trait Speak: Sync {
    fn word(&self) -> u32;
}

pub struct One;
pub struct Two;

impl Speak for One {
    fn word(&self) -> u32 {
        1
    }
}

impl Speak for Two {
    fn word(&self) -> u32 {
        2
    }
}

fn one() -> u32 {
    1
}

fn two() -> u32 {
    2
}

#[no_mangle]
pub static SPEAKERS: [&dyn Speak; 2] = [&One, &Two];

#[no_mangle]
pub static NAMES: [&str; 3] = ["one", "two", "three"];

#[no_mangle]
pub static FUNCTIONS: [fn() -> u32; 2] = [one, two];

#[cfg(tls)]
#[thread_local]
#[no_mangle]
pub static mut LABEL: &str = "label";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    loop {}
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
