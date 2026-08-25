---
status: open
kind: defect
opened: 2026-08-25
---

# `§N.N` citations in source point into a spec corpus the tree deleted

The spec documents were deleted by owner ruling; the deleting merge did not
sweep the source comments that cite their section numbers, so `§2.3a`, `§6.4`,
`§16.1` and kin survive as pointers at nothing — the exact rot the tracker's
deletion law exists to prevent, and no gate catches it because the marks live
in prose. The same corpus is also cited by bare label — `I5`, `C6`, `C12`,
`RT4`, `B10`, "invariant 10", "rejection 3" — and those are the same defect
wearing a different mark.

**Not every `§` is dead.** A citation into a live external document — Intel
SDM, xHCI 1.2, virtio 1.2, USB 2.0, the HDA specification, the PCIe base spec,
fatgen103 — is a boundary contract and stays. Only the marks into the deleted
internal corpus are the rot, and telling them apart is the work: an external
citation names its document, and an internal one says "spec §N", "§N's rule",
or cites a measurement no specification could carry.

The fix per site is the prose sweep's third wave's: say what the section said
in one clause, derived from what the surrounding code enforces, or delete the
sentence that leaned on it — never a bare unmarking, which keeps the sentence
and loses its ground.

## What is left

`rg -c '§[0-9]' $(git ls-files '*.rs')` is the finder. Run it rather than
quoting a number from here: concurrent sweeps move it, and it is a search bound
and never a score, because it counts the live external citations that stay
alongside the dead internal ones that are the defect.

By area:

- **`toyos-sched/`** — the prose is resolved. What survives there is not prose
  and cannot be reached by a comments-only sweep: two assertion strings
  (`loom/tests/loom_mailbox.rs`, `loom/tests/loom_sleep.rs`), the CLI usage
  text (`sim/src/main.rs`), and the identifier `crash_md_exit_race` together
  with the corpus trace named after it. Each is program output or a public
  name, so each needs a code-bearing change with its own gates.
- **`kernel/src`** — waves three and four resolved the files they held; the
  remainder is what concurrent sweeps were holding while they ran, plus the
  Intel SDM citations in `arch/` that are boundary contracts and stay.
- **the rest** — `toyos-xhci/`, `toyos-hda/`, `toyos-mixer/`,
  `toyos-fat32-check/`, `userland/`, `tests/` and `src/` carry the balance, and
  it is mostly the live kind: xHCI 1.2, the HDA specification, the PCIe base
  spec and fatgen103 are cited by section throughout. Telling those from the
  dead marks is the work.
