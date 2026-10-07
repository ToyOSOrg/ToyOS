---
status: open
kind: defect
opened: 2026-10-07
---

# A launch naming one handle twice becomes a direct spawn

`toyos::launch::launch` (`toyos/src/launch.rs`) answers `LaunchError::NotSent`
for a launch that names one handle twice or names the launcher's own
connection, before it consumes anything. std's `Command::launch`
(`rust/library/std/src/sys/process/toyos.rs`) answers every `NotSent` with the
direct spawn, and safe code reaches this one:
`Command::provide("a", h).provide("b", h)`.

So that caller is told nothing. Its program is spawned and not launched: it
holds its caller's namespace and not its manifest row, and neither `provide`d
name reaches it. `h` stays the caller's. A child asked of the supervisor
(`under_supervisor`) is refused `PermissionDenied` instead.

It is the fallback
`issues/a-launch-too-large-for-one-frame-becomes-a-direct-spawn.md` records for
a request that does not encode, reached by a second refusal.

Evidence: by reading. No test launches with one handle named twice.

Exit: std answers a `NotSent` launch that carries a `provide`d connector with
an error and starts nothing; a test that provides one handle under two names
reads that error and still holds the handle. Owner: the std lane, with
`issues/a-launch-too-large-for-one-frame-becomes-a-direct-spawn.md`.
