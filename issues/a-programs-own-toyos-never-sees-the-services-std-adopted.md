---
status: open
kind: defect
opened: 2026-11-12
---

# A program's own `toyos` never sees the services std adopted

std links a copy of `toyos` of its own (`sdk/toyos/src/lib.rs`, `std_io`),
and a program that uses the SDK links another. Each copy keeps its own
adopted services (`toyos::endow::adopt`, `ADOPTED` in
`sdk/toyos/src/endow.rs`), though `adopt`'s contract speaks of "this
process":
- std's `adopt_services` (`sdk/std/os/fs.rs`) fills std's copy, which std's
  file resolution and spawns read.
- The program's `toyos::endow::services` and `connector` read its own copy,
  which never adopted anything. Its `adopt` fills a set std never reads.

Seen in `adopted_services_stay` (guest run 43156966054, main 302fb3698): the
adopter's `services()` listed nothing after std had adopted `fs:/system`, and
the check on it went red; #1280 removed that check. The supervisor is the
one adopter today and keeps its own connectors, so nothing else reads the
wrong copy yet.

This is the defect #1226 closed for the stdio stream state, which each copy
kept for itself. That state now lives once, in the copy that owns the
descriptor table (the `fd-table` feature: std's in a Rust program, libc's in
a C one), which defines `toyos_stdio_*` beside `toyos_fd_*`, and every copy
reaches it through those.

**Owner**: the orchestrator, which assigns the fix after #1227 lands.

**Exit**: one adopted set per process, held by the copy that owns the
descriptor table and reached by every copy through it, as #1226's stdio state
is. A program's `toyos::endow::services` and `connector` then see what std
adopted, and an adoption through either copy is the one std resolves files
through.
