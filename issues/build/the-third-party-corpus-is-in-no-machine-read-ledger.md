---
status: open
kind: defect
opened: 2026-09-01
---

# 363 third-party source files are attributed in prose and by nothing that runs

`src/sourcegate.rs`'s `every_committed_binary_file_is_declared` covers every
committed file carrying a NUL, plus everything under `assets/` whether it does
or not. The clause it half-closes was written over *"every binary file git
tracks **plus every third-party source corpus**"*, and the corpus half is not
built.

The corpus is `tests/testcases/` --- 365 tracked files, of which **363** are
third-party across `tinycc/` and `pp_tcc/` (the other two are its own `LICENSE`
and a `system.toml` that is ours): TinyCC's `tests/tests2` and `tests/pp` under
LGPL-2.1 plus picoc under BSD-3-Clause. They are compiler *input* rather than
linked code, so their terms do not reach this repository's own, but the
attribution is still owed and
`NOTICE` says where it lives: `tests/testcases/LICENSE`, which says file by file
what is whose *"with the counts it was established from"*. Nothing checks that
those counts still describe the directory, so a file added to it is attributed
by a sentence that was true when somebody wrote it.

**Why it was not folded into the file scan.** The binary scan works because
`NOTICE` carries a digest per file and there are twenty-one of them. Here the
attribution is per *population* and the populations are named in a licence file
this repository did not write. The cheap form is the one that matches what is
actually claimed: read `tests/testcases/LICENSE`'s own counts, count the tracked
files under each subdirectory, and red when they disagree — which catches an
arrival and a deletion without pretending to judge any one file's provenance.

One case was deleted rather than attributed on 2026-08-08 —
`tests/testcases/tinycc/46_grep.c`, "Copyright (C) 1980, DECUS", *"but not for
profit"* — and `NOTICE` says not to re-import it. Nothing stops that either.

**And `assets/` is a directory, not a property.** The file scan reaches a
third-party text file only where it sits, so a byte-identical Phosphor SVG one
directory outside `assets/` arrives unremarked — measured 2026-09-02, green.
`NOTICE`'s paragraph about walking `assets/` whole is about `assets/` and says
nothing about anywhere else; the same walk, keyed on content rather than on
path, is what closes both halves of this entry at once.
