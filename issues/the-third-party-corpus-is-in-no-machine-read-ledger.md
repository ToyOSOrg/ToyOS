---
status: open
kind: tooling
opened: 2026-09-01
---

# The corpus is held to its counts and to nothing about any one file

`src/sourcegate.rs`'s `every_committed_binary_file_is_declared` covers every
committed file carrying a NUL, plus everything under `assets/` whether it does
or not. The clause it half-closes was written over *"every binary file git
tracks **plus every third-party source corpus**"*.

**Any one file's provenance is still owed.**

**And `assets/` is a directory, not a property.** The file scan reaches a
third-party text file only where it sits, so a byte-identical Phosphor SVG one
directory outside `assets/` arrives unremarked — measured 2026-09-02, green.
`NOTICE`'s paragraph about walking `assets/` whole is about `assets/` and says
nothing about anywhere else; the same walk, keyed on content rather than on
path, is what closes both of the halves left here at once.
