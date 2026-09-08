---
status: open
kind: defect
opened: 2026-09-08
---

# The boot parameter line's value is read in two crates

`toyos-abi/src/boot.rs` reads the kernel command line twice — `root_uuid` for
`root=` and `actuators` for the token list — and
`toyos-logstream/src/lib.rs`'s `value_in` is a third reading with `root_uuid`'s
body: `cmdline.split(',').find_map(|t| t.strip_prefix(P))`. There is one
question there and it is asked in two crates.

The reading belongs in `toyos_abi::boot` beside its two neighbours, as one
function taking the prefix, with `root_uuid` and `value_in` both calls to it.
What stopped that here is the branch rule: a commit touching `toyos-abi/src` may
not share a branch with anything else, and the record stream is not an ABI
change.

The exit condition is `toyos_abi::boot` growing that one function on an ABI
branch of its own, and `toyos-logstream` losing `value_in` in the branch that
follows it.
