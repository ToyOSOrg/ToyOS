//! A HID interface, bound by its report descriptor in report protocol, its
//! reports read through `toyos-usbhid` into the owner of input's records.
//!
//! **One decode path.** A boot-subclass interface is told `SET_PROTOCOL(Report)`:
//! "When initialized, all devices default to report protocol. However the
//! host should not make any assumptions about the device's state and should
//! set the desired protocol whenever initializing a device" (HID 1.11 §7.2.6),
//! and the request is "required only for boot devices" (§7.2). An interface
//! with a keyboard is told `SET_IDLE(0)` (§7.2.4), so a report comes only on a
//! change; one that stalls it reports at its polling rate, and an unchanged
//! report is no transition.
//!
//! **Each bound collection is one device to the owner**; one this reader
//! cannot bind costs only its own reports, counted and dropped.

use alloc::vec::Vec;

use toyos_device_memory::{DmaBuffers, Registers};
use toyos_usbhid::descriptor::{self as hid_descriptor, Kind};
use toyos_usbhid::report::{Counts, Hid, Read};

use crate::descriptor::{Interface, INTERRUPT, REPORT};
use crate::exec::{Dead, Host};
use crate::hc::{self, Done, Trb};
use crate::input::Handle;
use crate::usb::{Device, Setup};

/// The class requests (HID 1.11 §7.2): SET_IDLE and SET_PROTOCOL, to the
/// interface.
const SET_IDLE: u8 = 0x0A;
const SET_PROTOCOL: u8 = 0x0B;
const CLASS_TO_INTERFACE: u8 = 0x21;
/// GET_DESCRIPTOR of a class descriptor, from the interface (§7.1).
const STANDARD_FROM_INTERFACE: u8 = 0x81;

/// Refused reports said by name, the first few; the rest are counted.
const SAID: u32 = 4;

pub async fn serve<R: Registers, D: DmaBuffers, C: Host, T>(dev: &Device<'_, R, D, C, T>, interface: Interface) -> Result<(), Dead> {
    let usb = dev.usb;
    let at = (dev.port + 1, interface.number);
    let say = |line: core::fmt::Arguments<'_>| usb.say(format_args!("port {} interface {}: {line}", at.0, at.1));
    let Some(ep) = interface.endpoints.iter().copied().find(|e| e.kind == INTERRUPT && e.is_in()) else {
        say(format_args!("HID with no interrupt IN endpoint; not bound"));
        return Ok(());
    };
    let Some(length) = interface.report_bytes.filter(|n| usize::from(*n) <= hc::PAGE) else {
        say(format_args!("HID whose report descriptor length {:?} is not one page or less; not bound", interface.report_bytes));
        return Ok(());
    };
    let index = u16::from(interface.number);
    if interface.class.subclass == 1 {
        let report = Setup { request_type: CLASS_TO_INTERFACE, request: SET_PROTOCOL, value: 1, index, length: 0 };
        if let Err(why) = dev.control(report).await? {
            say(format_args!("SET_PROTOCOL(Report): {why:?}; not bound"));
            return Ok(());
        }
    }
    let ask = Setup { request_type: STANDARD_FROM_INTERFACE, request: 6, value: u16::from(REPORT) << 8, index, length };
    let bytes = match dev.control(ask).await? {
        Ok(bytes) => bytes,
        Err(why) => {
            say(format_args!("its report descriptor: {why:?}; not bound"));
            return Ok(());
        }
    };
    let parsed = match hid_descriptor::parse(&bytes) {
        Ok(parsed) => parsed,
        Err(why) => {
            say(format_args!("its report descriptor is refused: {why:?}; not bound"));
            return Ok(());
        }
    };
    let mut hid = Hid::new(parsed);
    let mut handles: Vec<Option<(Handle, Kind)>> = Vec::new();
    for (n, collection) in hid.collections().iter().enumerate() {
        let handle = match collection.bound {
            Ok(kind) => match usb.input.borrow_mut().bind() {
                Some(handle) => {
                    say(format_args!("collection {n}: a {kind:?}"));
                    Some((handle, kind))
                }
                None => {
                    say(format_args!("collection {n}: a {kind:?}, not bound: the owner's connection carries all it may"));
                    None
                }
            },
            Err(why) => {
                say(format_args!("collection {n} ({:#06x}:{:#06x}): not bound, {why:?}", collection.page, collection.usage));
                None
            }
        };
        handles.push(handle);
    }
    if handles.iter().flatten().any(|(_, kind)| *kind == Kind::Keyboard) {
        let idle = Setup { request_type: CLASS_TO_INTERFACE, request: SET_IDLE, value: 0, index, length: 0 };
        if let Err(why) = dev.control(idle).await? {
            say(format_args!("SET_IDLE(0): {why:?}; it reports at its polling rate"));
        }
    }
    let ended = read_reports(dev, ep.dci(), u32::from(ep.max_packet), &mut hid, &handles, &say).await;
    let mut input = usb.input.borrow_mut();
    for (handle, _) in handles.iter().flatten() {
        input.gone(*handle);
    }
    ended
}

/// Every report the endpoint delivers, until the device leaves or the
/// endpoint will not run again.
async fn read_reports<R: Registers, D: DmaBuffers, C: Host, T>(
    dev: &Device<'_, R, D, C, T>,
    dci: u8,
    max_packet: u32,
    hid: &mut Hid,
    handles: &[Option<(Handle, Kind)>],
    say: &dyn Fn(core::fmt::Arguments<'_>),
) -> Result<(), Dead> {
    let usb = dev.usb;
    let Some(page) = dev.page() else {
        say(format_args!("no page left in the grant for its reports; not read"));
        return Ok(());
    };
    let at = dev.hc().dma.device_addr(page);
    let length = max_packet.clamp(1, hc::PAGE as u32);
    let mut refused = 0u32;
    let mut counted = hid.counts;
    loop {
        if dev.leaving.get() {
            return Ok(());
        }
        let op = dev.submit(dci, &[Trb { param: at, status: length, control: hc::TRB_NORMAL | hc::TRB_ISP | hc::TRB_IOC }]);
        match op.wait(dev.wake()).await {
            Done::Gone => return Ok(()),
            Done::Event { code: hc::CC_SUCCESS | hc::CC_SHORT_PACKET, moved, .. } => {
                let report: Vec<u8> = (0..moved.min(length) as usize).map(|i| dev.read8(page + i)).collect();
                match hid.report(&report) {
                    Ok(Read::Keys { collection, .. }) => {
                        if let (Some((handle, _)), Some(held)) = (handles[collection], hid.held(collection)) {
                            usb.input.borrow_mut().keys(handle, held);
                        }
                    }
                    Ok(Read::Pointer { collection, report }) => {
                        if let Some((handle, _)) = handles[collection] {
                            usb.input.borrow_mut().pointer(handle, report);
                        }
                    }
                    Ok(Read::Unbound { .. }) => {}
                    Err(why) => {
                        refused += 1;
                        if refused <= SAID {
                            say(format_args!("a report refused: {why:?} ({report:02x?})"));
                        }
                    }
                }
                add(&usb.hid, &mut counted, hid.counts);
            }
            Done::Event { code, .. } => {
                if dev.restart(dci, None).await?.is_err() {
                    say(format_args!("its endpoint ended with code {code} and would not run again; it is let go of"));
                    return Ok(());
                }
            }
        }
    }
}

/// Fold what one interface counted since `seen` into the driver's total.
fn add(total: &core::cell::Cell<Counts>, seen: &mut Counts, now: Counts) {
    let mut t = total.get();
    t.reserved += now.reserved - seen.reserved;
    t.unlisted += now.unlisted - seen.unlisted;
    t.unsent_button += now.unsent_button - seen.unsent_button;
    t.unbound += now.unbound - seen.unbound;
    total.set(t);
    *seen = now;
}
