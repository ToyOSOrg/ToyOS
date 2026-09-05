---
status: open
kind: defect
opened: 2026-09-05
---

# Creating a symlink over a `/home` file leaves the file bound to the name

`BcacheFsAdapter::create_symlink` (`kernel/src/bcachefs_adapter.rs:374`) revokes
the name's blocks and then writes the link:

    self.revoke(name);
    mapped("create_symlink", name, self.fs.create_symlink(name, target))

`Mounted::create_symlink` (`bcachefs/src/fs.rs:765`) is `put`, which the crate
documents at `:769` as "displacing whatever answered to it" — so the file that
answered to `name` is gone from the volume. Its `name_to_id` binding is not:
`create_symlink` is the one path in this adapter that writes over a name on the
volume while a live binding for it stands. `create` (`:271`) returns the bound
id without touching the volume at all, so it cannot displace one; `delete`
(`:299`) unbinds before it asks the device; `ReplaceRename::release` (`:203`)
unbinds the displaced destination.

Nothing reads it back through that binding today, because `open` of a path whose
last component is now a symlink is resolved by `Vfs::resolve_for_open_depth`
(`kernel/src/vfs.rs:413`) into the link's target before `open_file` is reached.
That is a property of the resolver, not of this adapter: the invariant the map
is supposed to hold — a name is bound to at most one live file, and the file it
is bound to is the one the volume answers with — is false here, and the guard at
`close_file` (`:290`) that keeps a stale teardown from unbinding a live file
does not make it true, because nothing unbinds the displaced file at all.

## Reproduction

Not run, and there is no known guest-visible symptom to run for; the record is
the broken invariant and its one asymmetry against the four sibling paths.

## Exit condition

`create_symlink` unbinds the displaced name the way `create` and `delete` do,
with an arm that creates a symlink over an open `/home` file and asserts the
adapter answers for the link and not for the file — or a stated reason at the
site why this path alone may leave the binding, which the resolver's behaviour
is not, because it is not this module's to promise.
