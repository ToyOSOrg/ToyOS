---
status: open
kind: tooling
opened: 2026-09-29
---

# A compiler fixture's git commit could not create a temporary file

`src/compiler.rs`'s `estate` fixture, which
`one_compiler_per_key_whichever_checkout_names_it` and every other compiler
and LLVM test build on, failed under a test since deleted in 1 of 200 full
runs of the toyos-build lib test binary at `e3a1cdc8`. Those runs went beside a `cargo test --workspace --exclude
toyos-build` loop as host load, with the 1-minute load average at 62.82 for
that run. The fixture's own git failed, not the code under test:

    panicked at src/compiler.rs:373:9:
    git ["commit", "-qam", "a target"] in $TMPDIR/toyos-tmp-4827-0/compiler-record-29/fork-src:
    error: unable to create temporary file: Invalid argument
    fatal: failed to write commit object

The other 199 runs passed. Nothing has measured why git's temporary file in
that repository's object store failed with EINVAL.

## Exit condition

The EINVAL is traced to a cause the fixture can rule out by construction, and
a loop of 200 loaded runs shows every test that builds on it green each time.
