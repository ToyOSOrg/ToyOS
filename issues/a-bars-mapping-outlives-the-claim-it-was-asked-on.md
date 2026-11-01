---
status: open
kind: defect
opened: 2026-10-08
---

# A BAR's mapping outlives the claim it was asked on

Since #1137, a claim records every aperture it hands out: each BAR window
`pcidev::bar_object` makes, and each firmware window. `pcidev::revoke_handed`,
the one function every end of a claim calls, takes each back from every
space that maps it. It does so through the revoke an aperture's last handle
already does, so the old process's next access faults. It runs at the
claim's last handle (its release, its holder's death) and when the claim
moves, by IPC or a spawn's endowment.

**What is left:**
- **A function's departure.** #934 adds a function leaving (Gone). Whichever
  of #934 and #1137 lands second calls `revoke_handed` from it. Until then,
  a mapping of a function that departs while its claim is held outlives the
  departure. The next device behind that port may then be given the same
  BAR addresses.
- **No guest test reads it yet.** The arm is the one `bar_map_again`
  (`tests/toyos.rs`) boots: claim the virtio NIC, map a BAR, close the
  claim, claim again from a second process, and have the first process's
  read fault.

The tracker's ruling on taking a mapping back from a running process
(`issues/a-moved-handle-is-always-re-movable.md`, "Revocation is the other
answer") is that forced reclaim is killing the holder. A revoked mapping is
that: the old holder's next access to it faults.

**Exit condition**: after a claim ends, moves, or its function departs, no
mapping made through it reaches the function's registers, and a guest test
reads that.

**Owner**: whoever holds `issues/every-driver-is-still-in-the-kernel.md`.
