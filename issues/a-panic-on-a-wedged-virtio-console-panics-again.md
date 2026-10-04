---
status: open
kind: defect
opened: 2026-07-30
---

# A panic while the virtio-console TX queue is wedged *and* unlocked panics again, and the report never reaches the 16550

**The wait is no longer unbounded, and that is the whole of what changed.**
`Virtqueue::submit_and_wait` (`kernel/src/drivers/virtio.rs:611`) carries
`ANSWERS`, a `Tripwire::absurd(5 s, "far above any completion a live device
delivers")` (`:621`), and on expiry it panics by name (`:642`). A `Tripwire` is
the kind whose expiry *is* a panic (`kernel/src/time.rs:192`), so on the panic
path the bound converts a hang into a second panic — which is not the answer,
only a different wrong one. The heading this file carried until now said
"spins"; the tree refutes it.

The unlocked case is the one that reaches it. `panic_flush`
(`kernel/src/drivers/serial.rs:219`) spins for a clean handoff, and the
virtio-console `disable()` that would fall the report back to the 16550 sits at
`:237` — on the *other* branch, the one taken only when a holder never
releases. With the queue wedged but the guard free, the first `try_lock`
succeeds and `drain_locked` runs at `:227`; that drain is deliberately
unbounded in records ("the report should be whole",
`kernel/src/log/console.rs:111-113`), and each record goes out through
`BackendGuard::write_raw`'s `Backend::Virtio` arm (`serial.rs:147`) into
`virtio_console::write_bytes_locked` (`kernel/src/drivers/virtio_console.rs:95`)
and `submit_and_wait` (`:105`). Five seconds later `:642` panics inside the
panic handler, which re-enters `kernel/src/main.rs:114`, takes the reentry
guard at `:121`, and halts on `PANIC REENTRY: CPU halted` (`:124`).

So the machine still halts — the terminal action was never in question — but it
halts five seconds late, under a message that names the console rather than the
fault, with the real report truncated wherever the wedge caught the drain and
never delivered to the two channels that cannot wedge.

## The ruling, 2026-09-03: a panic never waits on a device

The panic path's wait is a `Budget`, not a `Tripwire` — the kind whose expiry is
a degraded answer and never a panic (`kernel/src/time.rs:222`); both kinds are
already in that file and nothing new is needed there. On expiry the console
output is **dropped**: `virtio_console::disable()`, then the panic proceeds to
the channels that cannot wedge — the 16550 (`serial::panic_raw`) and the panel
(`panic_console::render`) — which is exactly what `panic_flush`'s wedged-holder
branch at `serial.rs:234-240` already does for the other case. Terminal action
is unchanged: the machine still halts on the panic; it just no longer depends on
virtio answering to get there. The 5 s `Tripwire` stays what it is for every
non-panic caller of `submit_and_wait`; the panic path is the caller that may not
pay it.

**Not built here.** `kernel/src/drivers/virtio.rs` is inside the IOMMU pull
request's collision fence, so this file stays open and records what a fix owes.

## The two checks a fix owes

- **Negative control:** an actuator that wedges the TX queue — a device that
  takes the descriptor and never publishes to the used ring — with the backend
  guard left free, so the panic path enters `drain_locked` with a live
  `Backend::Virtio`. On the base that arm must show the `PANIC REENTRY: CPU
  halted` line and the truncated report; with the fix it must show the whole
  report and no reentry. Both arms run, both pasted.
- **Oracle:** the 16550 capture, which is not the channel under test. The
  wedged-queue arm's report arrives on a path that shares no code with virtio's
  queue, so a report that is whole there is evidence the drop happened and the
  panic proceeded, independent of anything the virtio driver reports about
  itself.
