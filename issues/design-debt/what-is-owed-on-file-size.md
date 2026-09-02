---
status: open
kind: defect
opened: 2026-08-08
---

# What is still owed on "this file is too big"

Nine of the owner's review notes asked whether a large file could become a
crate, a host test, or both. Eight are answered. The ninth's premise is gone and
what survives it is a different question.

**Answered:**

- `elf.rs` → `toyos-elf/` (host-tested, crafted-input corpus), `e2c6a06`; the
  mapping half stayed as `kernel/src/elf/`, `42b29c9`.
- `compositor/main.rs` → `toyos-desktop/` (pure), `763712b`; the effects shell
  is `userland/compositor/` over five files with a 71-line `main.rs`, `72705d9`.
- `xhci/mod.rs` → the port machine is `toyos-xhci/`, with a host simulator,
  `2e81ae8`.
- `soundd/main.rs` → `toyos-mixer/` (pure, `no_std`), 2026-08-20; the effects
  shell is `userland/soundd/` over eight files with a 261-line `main.rs`, from a
  2,366-line one. What the note asked for is what it got: **the mixing is
  sample-exact and proven so.** `toyos-mixer/fixtures/mix-corpus.txt` was
  written by `soundd/src/main.rs` before a line of it moved, and
  `the_corpus_is_reproduced_bit_for_bit` holds the crate to it byte for byte.
  The transcript is compact because exhausted domains are digested rather than
  listed: all 65,536 i16 both ways, all 65,538 quantizer ties, 65,536 dither
  draws from each of six seeds, all 184,001 client rates against twelve device
  shapes. 48 tests where soundd had eight, of which six were the ones that
  moved. Keeping the crate `no_std` cost a rounding function — `core` has no
  `f32::round` — and that one is held to `std`'s over all 4,294,967,296 f32 bit
  patterns.
- `loader.rs` → `kernel/src/loader/` (four files), `42b29c9`, with the pure
  decisions in `toyos-elf`. The plan/execute split — a `LoadPlan` an executor
  applies — is **not** built, and #159 changes what a mapping's protection is,
  so its shape is not settled.

**Answered 2026-08-24, in the review-completion wave:**

- `arch/syscall.rs` → `kernel/src/arch/syscall/` (12 files; `dispatch.rs` is
  where every user pointer the ABI takes is decoded — the seam the note asked
  for). A move-proof regenerated every new file from the original's line ranges;
  no function body changed. The split made two facts visible and filed:
  58 refusal `return`s exit past the epilogue, and a refused syscall is counted
  but not timed.
- `process.rs` — the lifecycle state machine is `toyos-proclife/` (pure,
  `forbid(unsafe_code)`, every interleaving of a scripted pair of paths
  enumerated; #142's mechanism class now has a host reproduction). Three
  subjects are still decided inside the file and reachable only by a booted
  guest: the handle/endowment build (`build_child_handles` +
  `Endowments::encode`, a crafted-input corpus's natural subject — the natural
  second chunk), `handle_page_fault`'s window arithmetic (~290 pure lines
  inside a function that also does device I/O), and the accounting
  (`stats_from`, `retire_threads`' merge, `PageFaultTrace`). #142 and #156
  remain the standing evidence.
- `symbols.rs` → `toyos-symbols/` (`no_std`, no alloc, `forbid(unsafe_code)` —
  stricter than the "core + alloc" expected; a real 1.6 MB ToyOS binary is the
  fixture, cross-checked against GNU readelf/nm). The raw-pointer `SymbolTable`
  read from fault handlers stays in the kernel, by design.

**The ninth: `drivers/acpi.rs`, and it is no longer a size.** Measured
2026-09-01, the file is 611 lines — outside the twenty largest this repository
tracks, against the 1,400 to 2,700 every answered entry started at. Better than
the note feared in kind as well: typed `TableError`, named bounds, packed
structs only for `offset_of!`. Nothing about it is owed to *this* record.

What the note was really reaching for outlives the size, and it is filed as
`issues/kernel/acpi-table-decoding-has-no-host-test.md`: the decode is over
firmware-supplied untrusted input and there is no host reproduction of any of
it.

**Promoted to `defect` 2026-08-25** (finding-lifecycle ruling; promoted **in
place** — this is the owner's review ledger and stays as the record of what the
nine notes asked and what answered them, rather than folding into any one
module). It stays open while `loader.rs`'s plan/execute split and
`process.rs`'s three remaining subjects are unbuilt; both are named above and
neither is a file-size claim any more either.
