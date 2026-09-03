---
status: open
kind: defect
opened: 2026-08-06
---

# Pulling the boot stick freezes the T14, and the diagnosis that was wrong

**The report.** Pulling the USB stick while the desktop is up freezes the whole
machine unrecoverably, from a USB-A or a USB-C port alike, and **Ctrl+Alt+D does
not answer afterwards**. That last clause is the strongest signal available: the
blocked-task dump is dispatched from `drain_irqs` at the top of a scheduler
pass, so no CPU is reaching a pass — not three of eight as in the wedge, all of
them.

**A diagnosis to withdraw, recorded because it read well.** The mechanism first
proposed was: every CPU entering a pass takes `XHCI`, one holds it across a full
blocking teardown with 2 s waits against a device that cannot answer, and the
rest spin. **It does not survive its own prediction.** `sync::Lock::lock` logs
`LOCK CONTENTION: {N}M spins` at 50M and panics `DEADLOCK` at 500M
(`kernel/src/sync.rs`); a `pause` iteration is tens of cycles, so a CPU waiting
behind one 2 s hold passes the warning and one behind two approaches the panic.
The owner reports a freeze with neither a contention line nor a panic screen. So
"every CPU spins on the ticket for seconds" is not what happened.

What the code still supports, and all it supports: **one CPU holding `XHCI` for
the transfer budget per SCSI command against a device that has gone.** Whether
anything else was spinning behind it is not settled by any evidence in hand.

**The residual that makes this hard, as a category.** The evidence channel is
the thing that fails. `/log` is on the stick being pulled, so the event that
would be diagnosed destroys its own record — a contention line goes into a ring
drained to a file on a device that is no longer there, and the T14 has no serial
port. **A defect whose evidence channel is the failing component cannot be
investigated by reading the log afterwards**, and this will not be the last one:
any device carrying `/log` has the same shape. What would break it is a channel
that does not depend on the storage stack — the on-screen panic console covers a
panic, and this is not one.

**What `c4ba7d5` closes.** The amplifier every candidate path shares.
`wait_transfer` ended on the clock; it now ends on the register when the slot's
port reads disconnected, because a device that has been unplugged is not a
device that is slow. A filesystem sync, a page-cache fill, a teardown and a
scheduler pass all reach that function, and pulling the stick a machine logs to
aims all of them at a dead device on one event.

**What it does not close, stated so a green suite does not imply otherwise:**

- ~~Teardown and `recover_endpoints` still block a pass~~ — **closed with the
  port machine**. Both are submit-and-return against one outstanding operation
  per controller: the pass that starts one gives itself back, and the completion
  arrives through the event ring the poll already drains. What is left on that
  path is `device::configure`; the type split that would make a wait there a
  compile error belongs with it,
  because a view that still has to hand `poll` a route to `configure` is a
  signature promising a check it does not perform. Two costs moved rather than
  going away, and neither is a defect: `PORT_WORK_AT` carries the outstanding
  operation's deadline, so an idle CPU declines to halt across a teardown
  exactly as it already does across a debounce, and a teardown now takes one
  further scheduler pass.
- **The metal claim is still the owner's to make.** Everything above is the
  guest-side proxy — no pass blocks — and the acceptance test is a stick pulled
  out of a running T14 with Ctrl+Alt+D still answering.
- `log_file`'s flush still holds `SINK` and the VFS across device I/O. The doc's
  "unbounded and uninterruptible" is half right, and the precise reading is
  **bounded in acquisition, unbounded in work**: `poll` is `try_lock` on both and
  disables the sink after `MAX_BLOCKED_NANOS`, so it never waits for a lock — but
  `Sink::flush` then calls `vfs.flush_file` and `vfs.sync_mount`, which reach
  `msc_write`/`msc_flush`, which take `XHCI` and spend the transfer budget per
  command.
- ~~There is no gate for the dangerous window~~ — there is now a gate for the
  *pull*, `usb_boot_stick_pulled`, and what it certifies is below. It is still
  not a gate for the 100 ms debounce and still cannot be aimed inside it.

**A negative result worth keeping.** The change did not make
`desktop_window_child` green; it stayed red across two landing gates. That is
evidence *against* the desktop freeze and the unplug freeze sharing the xHCI
path, and it agrees with the scheduler track's independent exclusion of the
ticket lock — two tracks reaching the same exclusion from different directions.

That makes three metal freezes with three triggers: a process reaching its first
instruction (~1.36 s), a keypress after 57 s of idle (86.9 s), and a stream's
first DMA (~3.8 s). The common factor is **something being scheduled or woken**,
which is #156's own title almost verbatim. Against the instrument as it first
stood all three would have read `heartbeats stopped at T` — a time and never a
class. With `ran=` they read as a time *and* one of two classes, which is what
makes a fourth flash worth more than the third was.
