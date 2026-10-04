---
status: open
kind: defect
opened: 2026-09-30
---

# A USB3 device attached but untrained reads as gone

Derived from the specification and the code, not staged and not seen on the T14.

A USB3 root-hub port that detected a device it could not bring to Enabled
reports it with CAS set and CCS clear (xHCI 1.2 §5.4.8, PORTSC bit 24), and a
warm reset clears it. `toyos_xhci::portsc::Portsc` names no CAS, and nothing
in `toyos-xhci` or `kernel/src/drivers/xhci` warm-resets a port for it. Such a
port reads `!Portsc::holds()`:

- the port machine (`toyos_xhci::port::PortState::step`) keys on CCS, so it
  tears down what the port held and never enumerates the device;
- the recovery ladder (`climb` and `reset_port` in
  `kernel/src/drivers/xhci/wait/msc.rs`) ends a rung whose stick is in that
  state as the stick leaving, and resets nothing.

So a disk whose device lands in that state is lost until it is replugged.
Linux reads CAS as a connection and warm-resets the port
(`xhci_hub_report_usb3_link_state` in `drivers/usb/host/xhci-hub.c`).

**Exit**: `Portsc` names CAS; the port machine warm-resets a USB3 port that
reads it and enumerates what trains; the ladder does not take such a device
for one that left.
