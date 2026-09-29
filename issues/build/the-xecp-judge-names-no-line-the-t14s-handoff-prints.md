---
status: open
kind: tooling
opened: 2026-09-29
---

# The xECP judge names no line the T14's handoff prints

`xhci_xecp` (`tests/toyos.rs`), `xhci_xecp_walk`'s judge on QEMU and on the
T14, finds the handoff by a line containing `USB Legacy Support` or
`ownership`. `take_ownership` (`kernel/src/drivers/xhci/legacy.rs`) prints
one of four lines, and only the one for a controller with no such capability
— QEMU's — contains either needle. The T14's controllers publish the
capability with firmware not owning them, and that line is
`xHCI: firmware did not claim the controller (USBLEGSUP 0x...)`.

## Measured

The full T14 run of `main` at `7e151819`
(EXIT=1): `FAIL xhci_xecp_walk: no line about the handoff at all:`. The
`selftests` readback's `kernel.log`
carries, for each of the two controllers, the self-test and the handoff
before the reset:

```
193:[2026-09-29 11:05:25 0.253 cpu0] xHCI: xecp selftest 8/8 malformed lists refused
194:[2026-09-29 11:05:25 0.253 cpu0] xHCI: firmware did not claim the controller (USBLEGSUP 0x01002201)
199:[2026-09-29 11:05:25 0.253 cpu0] xHCI: controller reset
```

## Exit condition

The judge recognises every line `take_ownership` can print about the
handoff, and a T14 run of `xhci_xecp_walk` passes; then this file is deleted.
