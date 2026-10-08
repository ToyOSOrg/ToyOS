---
status: open
kind: defect
opened: 2026-10-08
---

# A BAR's mapping outlives the claim it was asked on

`SYS_DEVICE_BAR_MAP` answers a `SharedMem` handle over a claimed function's
register window, carrying `MAP`, `DUP` and `TRANSFER` like any other
(`ops::initial_rights`). The object's life is its handles' and nothing ties it
to the claim: `pcidev::tear_down` (`kernel/src/pcidev/mod.rs`) stops the
function, resets it and takes its grants back, and touches no mapping of its
BARs. A window belongs to its BAR for the boot (`Machine::windows`), so the
next claim on the function is given the same address.

So a process that asked for a BAR, kept that handle — or sent it on — and let
the claim go still reads and writes the registers of a function its next
holder drives: it can aim that function's queues at any address the new
holder's grants map, which is the new holder's memory read out and written
through a device neither the unit nor the domain refuses.

**Read from the code, not run.** A guest arm is the one `bar_map_again`
(`tests/toyos.rs`) boots: claim the virtio NIC, map a BAR, close the claim,
claim again from a second process, and read the first mapping.

The tracker has ruled on taking a mapping back from a running process
(`issues/a-moved-handle-is-always-re-movable.md`, "Revocation is the other
answer"): forced reclaim is killing the holder. What that ruling means for a
window whose authority was a claim that has ended is not decided there.

**Exit condition**: after a claim ends, no mapping made through it reaches
the function's registers once another claim holds the function, and a guest
test reads that.

**Owner**: whoever holds `issues/every-driver-is-still-in-the-kernel.md`.
