---
status: open
kind: defect
opened: 2026-09-27
---

# A port given up on, or enumerated, after its change event is spent is never read again

The kernel steps its ports only when a Port Status Change Event arrived, or
when a port has work of its own (`Controller::poll`,
`kernel/src/drivers/xhci/mod.rs`). xHCI raises that event only on a change
bit's 0→1 edge, as QEMU's `xhci_port_notify` does. So a port that ends its work
in `Work::Settled` without reading the register again has spent its edge, and it
is looked at again only when the next edge arrives. Two paths end that way:

- **`GaveUp`.** A USB3 device pulled during the warm retrain that follows a
  failed bus reset: the retrain finds nothing, the port is given up on as
  attached, and `service_port` returns. The device's absence is never torn down.
- **The end of an enumeration.** A device pulled while Enable Slot is
  outstanding. Enable Slot is never cancelled, so when it ends silent `finish`
  reports the port attached and acknowledges its change flags, and nothing
  reads it again.

Both self-heal on the next plug's connect edge.

**The host simulator cannot show this.** `toyos-xhci/sim/src/driver.rs`'s
`pump` steps the port on every pass, and its hub raises no event at all. No
host test can fail on the gating inside `poll` either: moving the `outstanding`
test above `advance_outstanding` keeps every host suite green.

**Measured** with a patch that is not landed. It adds a pure
`port::due(signalled, ports)` that both loops call, a hub that raises an event
only on a change bit's 0→1 edge, and a pump that steps only where `due` says so.
Run with `cargo test -p toyos-xhci -p toyos-xhci-sim --all-features`:

- `superspeed::a_device_pulled_during_the_warm_retrain_is_not_enumerated` goes
  red with `[Reset(Hot), Reset(Warm), GaveUp(LinkNeverTrained)]` and no
  teardown.
- `enumerate::gate_an_enumeration_that_outlives_its_port_costs_a_deadline` goes
  red with "the port was never freed".
- A probe that pulls the device while Enable Slot is silent is green with
  per-pass stepping, and red with the edge rule, still attached with
  `[Reset(Hot), Enumerated { slot: None, trained: false }]`.
- `scenarios::repeated_replugs_stay_balanced` goes red on `port.rs` as of
  c5518949, with 1 teardown for 4 replugs. It is green once `torn_down` leaves
  the port `Work::Unread`.
- Three more lines in `toyos-xhci/src/port.rs` make every suite and the probe
  green: `enumerated` and both `GaveUp` transitions leave the port
  `Work::Unread`, not `Work::Settled`.

**Owner**: the xHCI driver, `kernel/src/drivers/xhci/` and `toyos-xhci/`.

**Exit**: `Controller::poll` and the simulator decide whether to step through
one function in `toyos-xhci`. The simulated hub raises an event only on a change
bit's 0→1 edge. No port reaches `Work::Settled` except from a read that found it
as the driver believes. The two tests above are green, and
`repeated_replugs_stay_balanced` is red on c5518949's `port.rs`.
