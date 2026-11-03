---
status: open
kind: defect
opened: 2026-10-08
---

# A BAR's mapping outlives the claim it was asked on

Since #1137, a claim records every aperture it hands out: each BAR window
`pcidev::bar_object` makes, and each firmware window. Each is taken back from
every space that maps it, through the revoke an aperture's last handle
already does, so the old process's next access faults:
- at the claim's last handle (its release, its holder's death), through
  `pcidev::revoke_handed`;
- when the claim moves, by IPC or a spawn's endowment. The move takes what
  the claim handed out under the lock that moves it, so the new holder's
  fresh mappings are not taken, and revokes it once that lock is let go.
- when its function departs from behind a hot-plug slot (#934), while its
  claim is still held: the device set's reader revokes what the claim handed
  out (`slot::refresh`), on its thread with no `MACHINE` or slot lock held,
  and only then wakes the holder. The claim keeps the list, and its release
  revokes it again before the function is given up, so the next device
  behind the port, which may be given the same addresses, is never reached
  through it. `a_function_arrives_and_leaves` reads a BAR handle the holder
  kept from before the pull mapping `Gone` once its departure woke it.

An aperture's handle is installed only while the caller's table still holds
the claim, under that same lock, so one a sibling thread asks for across a
release or a move is refused `Gone`.

**What is left:**
- **What no guest test reads.** `bar_map_again` (`tests/toyos.rs`) reads a
  BAR handle kept past an IPC move and past the claim's end mapping `Gone`,
  and the new holder's fresh mapping reading the device. Still unread: a
  live mapping faulting on its next access after a release or a move, a
  spawn's endowment, and the races the install check closes.

The tracker's ruling on taking a mapping back from a running process
(`issues/a-moved-handle-is-always-re-movable.md`, "Revocation is the other
answer") is that forced reclaim is killing the holder. A revoked mapping is
that: the old holder's next access to it faults.

**Exit condition**: after a claim ends, moves, or its function departs, no
mapping made through it reaches the function's registers, and a guest test
reads that.

**Owner**: whoever holds `issues/every-driver-is-still-in-the-kernel.md`.
