---
status: assigned
kind: defect
opened: 2026-10-12
---

# The tree names Linux as an authority

Root `CLAUDE.md`'s Zero legacy, `implementer.md`'s Measure, build, test and `reviewer.md`'s **Fit**
and **Tests** (#902): source, tests and manifests name Linux only as a host platform, or the name of
an interface defined outside ToyOS (a `libc` item, a firmware's `_OSI` string, a partition type),
nothing takes its authority from another implementation's source, and a test's oracle for what
hardware does is the vendor's document or ToyOS's own read checked against it, never a reading
captured under Linux. The tree breaks all three: a design cites Linux's source (`kernel/src/mm/mmio.rs`, `system/netstack/shard/tcp/src/rx.rs`), a value is set as Linux's
(`system/fileserver/src/resolve.rs`, `kernel/src/arch/x86_64/vtd/queue.rs`,
`sdk/libc/src/linkreq.rs`, `sdk/libc/src/sockopt.rs`, `sdk/libc/src/memreq.rs`,
`sdk/libc/src/posix_io.rs`, `sdk/libc/src/sigmask.rs`), and tests hold
ToyOS to Linux's code or readings (`system/netstack/i219/src/tests.rs`,
`lib/acpi/tests/thinkpad_t14.rs`, the T14 rows in `tests/toyos.rs` reading
`tests/metal/t14-linux/`).

Evidence: every mention outside `issues/` at `07b5879ee`,
[linux-mentions.txt](https://github.com/ToyOSOrg/evidence/blob/f42f8ab5a1107210c8e487963d9c530eb67741f4/linux-mentions.txt).

`kernel/cpuvuln` and its fixtures went in #915. The other kernel lines are
the sweep's: `kernel/src/arch/aarch64/irqchip.rs`,
`kernel/src/arch/x86_64/amdvi/mod.rs`, `kernel/src/arch/x86_64/vtd/fault.rs`,
`kernel/src/arch/x86_64/vtd/queue.rs`, `kernel/src/mm/mmio.rs` and
`kernel/usbhid`.

Owner: the orchestrator, which has briefed the source sweep
(`wt/toyos-linux-sweep`) to clear it.

**Exit**: `git grep -n -i linux -- kernel sdk/libc/src lib/acpi lib/hda lib/xhci lib/bcachefs
system/fileserver system/logkeeper system/netstack/i219 system/netstack/shard
system/netstack/node/tests tests/metal tests/toyos.rs tests/guest` prints nothing.
