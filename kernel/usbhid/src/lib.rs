//! USB HID reports in, key transitions and pointer motion out.
//!
//! Every byte here is one a device chose. A report that does not read as the
//! layout its interface promised is refused by name and changes no state, and
//! no input panics: [`keyboard::Keyboard::report`] and
//! [`pointer::Pointer::decode`] are total over every byte string.
//!
//! The layouts are fixed by the interface's protocol and read from no report
//! descriptor: HID 1.11 Appendix B's boot keyboard and boot mouse, and QEMU's
//! `usb-tablet`, which is what the kernel binds a subclass-0 interface as.
//! Nothing here touches a device, allocates, or holds a lock.

#![no_std]
#![forbid(unsafe_code)]

pub mod keyboard;
pub mod pointer;
