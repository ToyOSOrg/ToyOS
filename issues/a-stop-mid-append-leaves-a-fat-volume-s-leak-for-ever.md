---
status: open
kind: defect
opened: 2026-10-21
---

# A stop mid-append leaves a FAT volume's leak and stale free count for ever

A reset, panic or power loss inside a `toyos-fat32` call leaves what its
repair module's open windows name. On the T14's first unclean reset mid-append
(#971, `stick_survives_a_reset_under_load`) that was a log part's chain two
clusters past its size and `FSI_Free_Count` two clusters off. No byte the
size names was lost. `toyos-fat32-check` names both states, and the metal
readback accepts exactly those for a boot its deadline ends
(`toyos_fat32_check::unexplained_by_a_stop`).

Nothing on the mount path takes them back (`lib/fat32/src/repair.rs`, "The
repair lives in memory"). The clusters stay allocated and unreachable, and
the free count stays the hint it was, which `free_bytes` reports as the
volume's. Every unclean stop adds at most what one call claims.

**Exit**: a writable mount of a volume that was not cleanly stopped takes
back what a stop can leave: lost chains freed, a chain too long cut to its
size, the FAT copies reconciled, and the free count recounted. It knows the
volume was not cleanly stopped from fatgen103's ClnShutBitMask (FAT[1]),
cleared while writes are open and set again at sync. Shown by the fileserver
crash test mounting each stopped state writable and the checker then finding
nothing. Owner: `lib/fat32`, held by the orchestrator.
