---
status: open
kind: finding
opened: 2026-10-21
---

# The key record the surface protocol carries is defined in the kernel ABI

`RawKeyEvent`, the `MOD_*` flags and the pointer kinds live in
`sdk/abi/src/input.rs`, the kernel/program boundary, and the kernel no longer
speaks any of them: its keyboard and mouse streams went with the small-kernel
track's step 10. Their readers are the owner of input's wire
(`lib/input/src/wire.rs`), the surface protocol (`sdk/toyos/src/surface.rs`,
`sdk/window`) and the compositor's desktop. A userland protocol owns its wire
types in its own library (root `CLAUDE.md`), so these belong to the surface
protocol and `toyos-input`, not to `toyos-abi`.

**Exit**: `sdk/abi` defines no input record, and the surface protocol and
`toyos-input` own theirs. Owner: `lib/input`, held by the orchestrator, beside
the input track's text (#904).
