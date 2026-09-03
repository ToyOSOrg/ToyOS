---
status: open
kind: defect
opened: 2026-09-01
---

# The corpus is held to its counts and to nothing about any one file

`src/sourcegate.rs`'s `every_committed_binary_file_is_declared` covers every
committed file carrying a NUL, plus everything under `assets/` whether it does
or not. The clause it half-closes was written over *"every binary file git
tracks **plus every third-party source corpus**"*.

The corpus is `tests/testcases/` --- 365 tracked files, of which **363** are
third-party across `tinycc/` and `pp_tcc/` (the other two are its own `LICENSE`
and a `system.toml` that is ours): TinyCC's `tests/tests2` and `tests/pp` under
LGPL-2.1 plus picoc under BSD-3-Clause. They are compiler *input* rather than
linked code, so their terms do not reach this repository's own, but the
attribution is still owed and
`NOTICE` says where it lives: `tests/testcases/LICENSE`, which says file by file
what is whose *"with the counts it was established from"*.

**The counts half runs now.**
`sourcegate::tests::the_corpus_matches_the_counts_its_own_licence_declares`
reads that licence's own per-population numbers, counts what `git` tracks under
each population, and reds when they disagree; it also refuses a tracked file
under the corpus that no population attributes, and the one name `NOTICE` says
is not to come back --- `tests/testcases/tinycc/46_grep.c`, "Copyright (C) 1980,
DECUS", *"but not for profit"*, deleted rather than attributed on 2026-08-08. An
arrival and a deletion are both caught.

**Any one file's provenance is still owed.** A file swapped for another under
the same name, or any substitution that leaves both counts intact, passes. The
counts scan cannot judge that: the binary scan works because `NOTICE` carries a
digest per file and there are twenty-one of them, and no digest per corpus file
has ever been established.

**And `assets/` is a directory, not a property.** The file scan reaches a
third-party text file only where it sits, so a byte-identical Phosphor SVG one
directory outside `assets/` arrives unremarked — measured 2026-09-02, green.
`NOTICE`'s paragraph about walking `assets/` whole is about `assets/` and says
nothing about anywhere else; the same walk, keyed on content rather than on
path, is what closes both of the halves left here at once.
