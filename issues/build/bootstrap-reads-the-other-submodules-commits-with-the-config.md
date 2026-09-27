---
status: open
kind: tooling
opened: 2026-09-27
---

# Bootstrap reads the other submodules' commits with the config

`rust/src/bootstrap/src/core/config/config.rs`'s `parse_inner` takes the
`GitInfo` of `src/tools/cargo`, `clippy`, `miri`, `rustfmt`, `rust-analyzer`,
`src/tools/enzyme` and `src/gcc` before `update_existing_submodules` moves any
checkout, and keeps it for the run. A submodule whose recorded commit moved
while its checkout stayed put is described at the commit it was at: in the
tools' version strings, and in Enzyme's build stamp (`enzyme_info` in
`core/build_steps/llvm.rs`). `src/llvm-project`'s was the same defect until the
fork read it when asked (`in_tree_llvm_sha`). Whether any of these reaches a
ToyOS build is not measured.

**Exit**: each is read when asked, as `src/llvm-project`'s is.
