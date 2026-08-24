---
status: open
kind: defect
opened: 2026-08-02
---

# A port that fails its reset gets no second try, and no warm reset

xHCI 1.2 §4.19.5: "Only a USB3 protocol port may fail the bus reset sequence.
USB2 protocol ports never fail." A USB3 port that does fail comes back with
PLS = RxDetect, PRC set, the speed field zero and **CCS cleared** — so the
failure is distinguishable from success at the register, and `init_device`
distinguishes nothing: it checks PED, logs `reset but not enabled` and drops the
port for the life of the boot.

The spec's answer is §4.19.5.1, a Warm Port Reset: software writes WPR (bit 31)
instead of PR, which resets the USB3 link itself rather than only the device.
This driver never writes WPR, and `PORTSC_RWS` deliberately excludes it, so
there is no path to one. Linux retries either way — `PORT_RESET_TRIES` is 5 and
`PORT_INIT_TRIES` 4 in `drivers/usb/core/hub.c`, and `hub_port_reset` escalates
a failed hot reset to a warm one.

Doing this properly needs the Supported Protocol capability, because
"retry as a warm reset" is only correct on a USB3 port and WPR is RsvdZ on a
USB2 one. It costs a device on a receptacle whose link does not train first
time; on the T14 the receptacle in question is the one the boot stick is in.
Nothing in QEMU can fail a reset — `xhci_port_reset` sets PED for every speed it
knows and never takes the failure path — so this needs an actuator of its own,
and `xhci-portsc-rw1c`'s shape (replace what the register reads) is the one that
fits.

**2026-08-25: the capability this named as unbuilt is now built.**
`kernel/src/drivers/xhci/wait/boot.rs`'s `read_protocols` decodes the
Supported Protocol capability, `toyos-xhci/src/port.rs`'s `reset_needed`
consults it, and both `kernel/src/drivers/xhci/wait/boot.rs`'s `init_device`
and the hot-plug port state machine in `kernel/src/drivers/xhci/mod.rs` now
dispatch `Reset::Warm` and retry a failed hot reset on a USB3 port before
giving up (`GaveUp::LinkNeverTrained`). Whether that closes this entry or only
narrows it — `device::begin`'s own PORTSC check in `kernel/src/drivers/xhci/device.rs`
still has no retry of its own — was not verified further; found while folding
`issues/hardware/xhci-legacy-handoff-unstageable.md`, the finding this
paragraph used to cite for "unbuilt", and left for whoever next reviews this
entry.
