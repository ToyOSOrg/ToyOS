---
status: open
kind: defect
opened: 2026-09-29
---

# The i8042's last ISR can read a byte its new holder owns

`i8042::quarantine` (`kernel/src/arch/x86_64/i8042/mod.rs`) masks GSI 1 and 12
and then lets the controller go, and an `isa` claim may be granted from that
moment. The mask stops new edges at the I/O APIC, not an edge the local APIC
already holds in IRR for the driver's vector, nor a `handler` already running
on `IRQ_CPU`: a flood re-edges on every `0x60` read, so one such edge is the
usual case. That handler reads port 0x60 up to `ISR_BURST` times while OBF is
set, so a byte that arrives for the claim's holder before it runs goes into the
kernel's ring, which nothing drains any more, and the holder never sees it.
Under `i8042-fault` it reads sixteen bytes whatever OBF says.

The flood itself can no longer run the quarantine twice (the flood is taken at
most once per boot); this is the handler's port read, which nothing orders
against the grant. No test drives a byte into that window.

**Exit**: no read of port 0x60 by the kernel's handler can follow a granted
`isa` claim on the i8042 — the grant waits on the driver's vector being idle on
`IRQ_CPU`, or the handler refuses the port once the driver has let go — with a
guest test that stages the late handler and must see the holder's byte.
