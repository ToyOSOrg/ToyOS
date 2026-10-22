//! USB HID reports in, key transitions and pointer reports out, through each
//! interface's own report descriptor.
//!
//! Every byte here is one a device chose. [`descriptor::parse`] reads the
//! descriptor into its top-level collections, refusing by name what it cannot
//! read, and [`report::Hid`] reads each report through it: one that does not
//! read as its descriptor says is refused by name and changes no state, and
//! no input panics. Nothing here touches a device or holds a lock.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod descriptor;
pub mod report;

#[cfg(test)]
mod tests;
