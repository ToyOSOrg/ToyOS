---
status: open
kind: defect
opened: 2026-10-02
---

# A launch too large for one frame becomes a direct spawn

`toyos::launch::launch` (`toyos/src/launch.rs`) answers `LaunchError::NotSent`
when `Launch::encode` refuses the request, and std's `Command::launch`
(`rust/library/std/src/sys/process/toyos.rs`) answers `NotSent` with the direct
spawn. `encode` refuses a request whose header, program, argv, environment,
working directory and connector names do not fit `MAX_FRAME_LEN`
(`toyos/src/ipc.rs`). Its other refusal, too many connectors or slots, std
never reaches: it refuses the connectors itself, and a launch carries stdio
alone.

So a declared program started with an argv and environment past one frame is
not launched. Its caller spawns it: it holds its caller's namespace and not its
manifest row, a connector the caller `provide`d does not reach it, and it
carries no `HOME` unless its caller named one. Neither side is told. A child
asked of init (`under_init`) is refused `PermissionDenied` instead.

Evidence: by reading. No test sends a launch past one frame.

Exit: a launch `encode` refuses answers its caller an error and starts nothing,
or the wire carries every argv and environment `SYS_SPAWN` takes; a test
launching a declared program with an environment past one frame reads that
error, or finds the child holding its row. Owner: the std lane, with
`toyos/src/launch.rs`.
