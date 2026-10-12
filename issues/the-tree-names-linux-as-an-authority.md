---
status: assigned
kind: defect
opened: 2026-10-12
---

# The tree names Linux as an authority

Root `CLAUDE.md`'s Zero legacy, `implementer.md`'s Measure, build, test and `reviewer.md`'s
**Fit** and **Tests** (#902): source, tests and manifests name Linux only as a host platform or an
interface the `libc` crate defines, nothing takes its authority from another implementation's
source, and a test's oracle for what hardware does is the vendor's document or ToyOS's own read
checked against it, never a reading captured under Linux. The tree breaks all three: a design
cites Linux's source (`kernel/cpuvuln`, `kernel/src/mm/mmio.rs`,
`system/netstack/shard/tcp/src/rx.rs`), a value is set as Linux's
(`system/fileserver/src/resolve.rs`, `kernel/src/arch/x86_64/vtd/queue.rs`), and tests hold
ToyOS to Linux's code or readings (`system/netstack/i219/src/tests.rs`, `kernel/cpuvuln`'s
fixtures, `lib/acpi/tests/thinkpad_t14.rs`, the T14 rows in `tests/toyos.rs` reading
`tests/metal/t14-linux/`).

Evidence: every mention outside `issues/` at `07b5879ee`,
[linux-mentions.txt](https://github.com/ToyOSOrg/evidence/blob/f42f8ab5a1107210c8e487963d9c530eb67741f4/linux-mentions.txt).

Owner: the orchestrator, which has briefed two branches to clear it: the source sweep
(`wt/toyos-linux-sweep`) and the `kernel/cpuvuln` design (`wt/toyos-cpufacts`).

**Exit**: `git grep -n -i linux -- kernel lib/acpi lib/hda lib/xhci lib/bcachefs system/fileserver
system/logkeeper system/netstack/i219 system/netstack/shard system/netstack/node/tests tests/metal
tests/toyos.rs tests/guest` prints nothing.
