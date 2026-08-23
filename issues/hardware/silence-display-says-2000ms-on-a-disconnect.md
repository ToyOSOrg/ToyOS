---
status: open
kind: defect
opened: 2026-08-23
---

# `Broke::Silence` says "no answer in 2000 ms" on exits that waited no 2000 ms

`kernel/src/drivers/xhci/wait/msc.rs`'s `Broke::Silence` renders as
`no answer in the {phase} phase in 2000 ms` (`USB_TIMEOUT_NS`), but
`wait_transfer` has exits that return `None` without the bound elapsing:

- `kernel/src/drivers/xhci/wait/mod.rs`'s disconnected-port check — a pulled
  stick is logged as a timeout. (Carried over as the one residual when the
  slow-vs-failed flush issue closed on 2026-08-23; git has that file's story.)
- The two staged exits, `usb-transport-break` and `usb-reset-break`, which
  skip the wait by design; their logs carry the same false "2000 ms".

The fix shape: `wait_transfer` says *why* it returned `None` (elapsed,
disconnected, staged), and `Silence`'s `Display` renders that instead of
asserting the constant. Cosmetic in the shipped kernel until a stick is pulled
mid-transfer — and on the T14, where the log is the only channel, a pulled
stick reading as a slow one sends a triage to the wrong shelf.
