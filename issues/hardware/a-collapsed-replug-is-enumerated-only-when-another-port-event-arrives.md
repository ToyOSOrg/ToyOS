---
status: closed
kind: defect
opened: 2026-09-27
---

# A collapsed replug is enumerated only when another port event arrives

A device pulled and pushed back between two looks is torn down
(`Step::Teardown(Gone::Replugged)`), and its slot's Disable Slot completes in a
later `Controller::poll` (`kernel/src/drivers/xhci/mod.rs`). There
`slot_gone`'s `AfterSlot::Teardown` calls `PortState::torn_down`, which leaves
the port `Settled` and not attached with the device still in it. `poll` steps
the ports only when `ports_dirty` is set or a port is `outstanding`. Neither is
true then, so nothing looks at the port again. `PORT_WORK_AT` goes to 0, and
PORTSC's CSC stays set because the teardown step returns ahead of the
acknowledge. QEMU raises no further Port Status Change for that port
(`xhci_port_notify` returns while the bit is set), so the device in the port is
never enumerated. It stays dead until an unrelated event marks the ports dirty.
The next unplug is one such event. On the T14 this is a replugged mouse that
does not come back.

Whether the wake is lost depends on how QEMU's two edge events fall across
polls: `xhci_port_update` clears PORTSC and then notifies, so the detach and
the attach each raise their own event. The wake survives when the attach's
event is drained in the same poll as the completion or later. It is lost when
both events are drained before the completion.

## Evidence

- The PR #535 nightly (run 36314576406, `guest (1)`, a4f68c5a) was red with
  `0 slot(s) enabled and never disabled ([]) after 4 replugs`. The first three
  collapses re-enumerated 100 ms after their teardown (1.780 → 1.881 s). The
  fourth was torn down at 3.586 s, and nothing followed in the 900 ms before
  the guest's input ended. `wt/toyos-lld` at a55d62c6, an ancestor of `main`
  (run 36287592139, `guest (1)`), was red with the same sentence. Its first
  collapse (1.971 s) was enumerated only at 2.674 s, 700 ms later, when the
  next cycle's edges arrived.
- The dev host, QEMU 11.1.1, TCG, on `nightly-green2` at 877b8c95. QEMU's
  `hw/usb/hcd-xhci.c` is byte-identical at v11.1.0 and v11.1.1. With only a
  print of the serial added, every other collapse sits about 700 ms until the
  next cycle rescues it: 1.022 → 1.725 s and 2.232 → 2.938 s. The test is
  green because the fourth cycle is a rescue. With `self.ports_dirty = true;`
  added after `torn_down()` in `AfterSlot::Teardown`, all four collapses are
  seen as such and each re-enumerates 100 ms after its teardown
  (`4 replugs collapsed inside the debounce (4 seen as such): 5 slot(s)
  enabled`).
- The same tree with `CYCLES = 3`: red, `EXIT=1`, `0 slot(s) enabled and never
  disabled ([]) after 3 replugs`, red again in the harness's alone re-run.
  With the one-line wake added it is green, `EXIT=0`, `3 seen as such`.
- One named run of `xhci_flap` as committed is green on `nightly-green2`
  (`EXIT=0`) and on `main` at 16d2e645 (`EXIT=0`).
- PR #542's nightly at 059c5de7 (run 36328646395, `guest (2)`), whose diff
  touches no driver or guest code, was red with the same sentence. The
  collapse torn down at 1.523 s was enumerated at 2.225 s, when the next
  cycle's edges arrived; the one torn down at 3.328 s had nothing after it
  before the guest's input ended at 4.234 s.

So the gate as committed passes by parity wherever every collapse loses its
wake. On the dev host it cannot go red. It reds on CI's KVM shards only when
the last collapse is the one that loses it.

`AfterSlot::Again` ends in the same `torn_down()` with no look after it; that
arm was not staged.

## Exit condition

A port torn down with its device still in it is looked at again without
waiting for another event, shown by a gate that goes red on the lost wake on
every host. `xhci_flap` at an odd cycle count is one such gate. An assertion
that every collapsed teardown is enumerated before the next cycle's edges is
another. `xhci_flap` is disabled in `src/redlist.rs` until then, and the change
that meets this deletes its row. Owner: the xHCI driver's port stepping
(`kernel/src/drivers/xhci/mod.rs`); held by the orchestrator.

Closed: `PortState::torn_down` (`toyos-xhci/src/port.rs`) leaves the port `Unread`, which `PortState::outstanding` reports, so the next `XhciController::poll` reads it without waiting for an event.
