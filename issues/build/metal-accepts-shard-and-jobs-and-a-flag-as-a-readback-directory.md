---
status: open
kind: tooling
opened: 2026-09-29
---

# `--metal` accepts `--shard` and `-j` and reads neither

`cargo test --test toyos-build -- --metal --shard 2/4` and `--metal -j 4` are
accepted by `testargs::parse` and the metal dispatch reads neither flag: the
run continues as if it were absent, in silence, the same shape as the
`--nightly`/`--weekly` refusal beside it.

`--metal --metal-readback --list` is also accepted: the readback flag takes the
next word as its value, so the directory is named `--list` (the mode is still
`List`, but `--metal --metal-readback --bogus-dir-flag` would stage into it).

Exit: `parse` refuses `--shard` and `-j`/`--jobs` alongside `--metal` by name,
and refuses a `--metal-readback` value that begins with `-`.
