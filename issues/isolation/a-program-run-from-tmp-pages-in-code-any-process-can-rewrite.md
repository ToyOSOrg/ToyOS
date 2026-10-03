---
status: open
kind: defect
opened: 2026-10-03
---

# A program run from `/tmp` pages in code any process can rewrite

A spawn by path of a `/tmp` file maps each `PT_LOAD` segment as a file-backed
region over a `TmpfsBacking` (`insert_elf_regions`, `kernel/src/loader/mod.rs`),
and a fault fills its 2 MiB window from the file cache as the file is at that
fault (`handle_page_fault`, `kernel/src/process.rs`; `TmpfsBacking::read_page`,
`kernel/src/tmpfs.rs`). Nothing copies the file at the spawn and nothing refuses
a write to it while a process pages from it, so a write after the spawn reaches
every window of the running program not yet faulted in, its code included.
`/tmp` is writable by every process, so the writer need not be the spawner: one
program changes another's code. `SharedImage` (`kernel/src/file_backing.rs`) has
the same shape on the image route and is bounded there by who can write: the
object is the spawner's own. `issues/isolation/a-swapped-binary-lives-where-any-process-can-rewrite-it.md`
is this defect for the swap's installed binaries.

By reading, unmeasured.

Owner: orchestrator. Exit condition: a test spawns a `/tmp` executable, rewrites
a window of its code the child has not yet run, then has the child run it, and
reds while the child runs the rewritten bytes.
