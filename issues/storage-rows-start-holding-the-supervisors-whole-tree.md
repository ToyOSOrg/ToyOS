---
status: open
kind: defect
opened: 2026-11-11
---

# Storage rows start holding the supervisor's whole-tree namespace

By inspection, and unverified on a machine: `diskserver` and the file servers
for the `memory` and `system` roles are started holding a duplicate of the
supervisor's own namespace, every directory of the tree read-write but
`/system`; `/apps` and `/log` are among them.

The supervisor gives a storage row no view (`Grants::view` in
`system/supervisor/src/main.rs`), since a file server resolving a path
through itself waits for ever. Where such a row has no other name either,
`build_namespace` answers no namespace at all, and a spawn that names none
endows a duplicate of the spawner's own (`Helper::spawn`,
`sdk/toyos/src/spawn.rs`). The supervisor's own is the whole tree
(`toyos_manifest::whole_tree`).

All three are the signed image's storage stack and open no path, so no
boundary between a session's programs and `/apps` or `/log` depends on it.
But "a storage row sees none" is not what happens, and no other row is
started this way.

Evidence owed: `names.open("fs:/apps")` from one of those processes, which
answers a connection if this is right; if not, the process holds no
namespace, or one without `fs:/apps`.

Exit: a storage row is started with an empty namespace where its row names
nothing, the check above refused on a machine, as part of stage 2 of
`issues/every-program-sees-only-the-files-it-was-given.md`, whose exit is
that nothing still sees the global tree.

Owner: the file-isolation track.
