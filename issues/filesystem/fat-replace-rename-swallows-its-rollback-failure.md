---
status: open
kind: defect
opened: 2026-08-31
---

# A FAT replacing rename discards its own rollback's failure, leaving the destination alive only under the staging name

`Fat32::replace_rename` (`toyos-fat32/src/fs.rs:897`) stages an existing
destination under `.toyos-replaced-{sequence:08x}.tmp`
(`toyos-fat32/src/fs.rs:922`), moves the source onto the freed name, and frees
the staged entry only after the move commits. When the move fails it restores
the destination — and discards the restore's own result:

    toyos-fat32/src/fs.rs:906:            let _ = self.rename(&temporary, to);

If that restore hits a device error too, `to` is absent, the destination's data
and clusters are alive under the staging name, and the error the caller gets is
the *move's* cause. Nothing names the staging file, so nothing can find it back.
`ReplaceRename::commit`'s contract in `kernel/src/fs_rename.rs:38` — "on `Err`
nothing durable moved" — then holds only when the rollback rename succeeds,
which is exactly the case the rollback exists for.

Reproduced against the real writer with a `BlockAccess` device that refuses the
source's short-entry write and then the rollback's, over a volume built by the
fatgen103-derived builder in `toyos-fat32-check` (throwaway test, not committed):

    REPLACE_RENAME_RESULT=Err(Io)
    source.txt exists = Ok(true)
    destination.txt exists = Ok(false)
    ROOT ENTRY: SOURCE.TXT
    ROOT ENTRY: .toyos-replaced-00000000.tmp
    refused_source=true refused_rollback=true
    CHECKER_COMPLAINTS=

The last line is the reason this is silent rather than loud: the volume is
structurally valid, so `toyos_fat32_check::check` has nothing to complain about
and no fsck pass will ever surface it. Nothing sweeps a stale
`.toyos-replaced-*.tmp` either — `replacement_temporary` only skips names that
are taken, so a leftover consumes one of `MAX_DIR_ENTRIES` sequences and stays.

This is strictly narrower than the window the staging order was built to close:
that one lost the destination outright on a *single* device error during an
overwrite. This one needs a second device error, during the rollback, and does
not lose the data — it only stops anybody from naming it.

**Exit condition.** `replace_rename` reports a failed restore instead of
discarding it: the caller learns that `to` is absent and learns the staging name
that holds it, distinguishably from the case where the rollback put the
destination back. A regression drives the two-refusal device above through the
real `Fat32` and asserts the reported error carries the staging name, with
`toyos_fat32_check::check` still finding no fault.
