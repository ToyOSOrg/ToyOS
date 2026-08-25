---
status: open
kind: defect
opened: 2026-08-25
---

# On macos-latest, the libc merge reads an rlib that is not an ar archive

The portability instrument's second macOS reading (dispatch run 32787940769,
2026-08-25) got past the doom link the first reading found and died one step
later, at "Building toyos-libc for sysroot...":

```
thread 'main' panicked at src/libc.rs:154:5:
not an ar archive
```

`merge_rlibs` reads the rlibs the fork's own rustc just produced for the
toyos target and one of them does not begin `!<arch>\n`. That magic is shared
by GNU and BSD archives alike, so this is not the ar-dialect class #283
closed — the file is not an ar archive at all (a thin archive, an empty file,
or something else entirely).

What makes it a defect rather than a shrug: the same path runs green on the
aarch64-apple-darwin dev host every day, so the difference is specific to the
hosted runner's bootstrap and nothing recorded yet says what. The assert
printed neither the path nor the bytes it saw, so the run that fired it left
no way to tell — the diagnostic half is fixed alongside this filing (the
refusal now names the file and its first eight bytes), and the next macOS
nightly answers the question this file asks.
