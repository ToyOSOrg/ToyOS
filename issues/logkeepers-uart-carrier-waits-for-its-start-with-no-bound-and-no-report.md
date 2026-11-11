---
status: open
kind: defect
severity: [hang]
opened: 2026-11-11
---

# logkeeper's UART carrier waits for its start with no bound and no report

The console UART's carrier is fed from where the kernel left the UART, which
`logkeeper` says once its read positions reach what the kernel says the
console spoke (`system/logkeeper/src/main.rs`, `set_wire_from`). Until then
the carrier's feed waits in `Hub::wire_start`
(`system/logkeeper/src/serve.rs`), on a condition with no timeout, and says
nothing.

If the start is never said, the UART carries nothing for the machine's life
and no line anywhere says why. That is what a kernel answer that could not
be reached did on a machine with a CPU that never started, until the answer
was made reachable: the cause went, this wait stayed.

**Exit**: the wait has a bound; past it `logkeeper` says so and starts the
carrier anyway, from the hand-over with what the rounds before it owe. Lines
the kernel already put on the UART may then repeat, which the port's `STALL`
already holds is better than a silent UART. A host test of the carrier's
start holds the expiry.
