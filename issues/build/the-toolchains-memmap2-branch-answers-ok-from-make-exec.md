---
status: open
kind: defect
opened: 2026-09-26
---

# The toolchain's memmap2 branch answers `Ok` from `make_exec`

memmap2's `toyos` branch, which `rust/Cargo.lock` pins at 87ae85a1 for
`rustc_data_structures` and `measureme`, still answers `Ok(())` from
`MmapMut::make_exec` on a heap buffer the kernel never made executable.
`toyos-0.9.11`, the branch the tree's own workspaces consume, answers
`Unsupported` as `map_exec` does. Nothing in the toolchain calls
`make_exec`, and `rustc_data_structures` gates `target_os = "toyos"` away
from memmap2 entirely, so no caller is misled today; the branch still says
something false.

Appending the fix to `toyos` alone would leave `rust/Cargo.lock` one commit
behind the branch, which `cargo run -- --check-forks` reports as a drift, and
re-locking `rust/Cargo.lock` is a change to the compiler's own dependency
graph.

**Exit**: the fix appended to memmap2's `toyos` branch in the same change
that re-locks `rust/Cargo.lock` onto it.
