---
status: open
kind: tooling
opened: 2026-10-01
---

# The toolchain release is compressed at zstd's fastest level

`release::pack` (`src/release.rs`) compresses the release's tarball in-process
with ruzstd 0.9.0, whose encoder implements one level, `Fastest`, which its
documentation calls roughly zstd's level 1. The zstd binary that packed it
before compressed at level 3. Measured on the development host (macOS, arm64)
over one 1,369,952,256-byte tarball of sysroot `3b3ed0fb252fe96d`: ruzstd
wrote 566,444,661 bytes, the `zstd` CLI 379,391,222 at `-3` and 418,323,664 at
`-1`. Every consumer outside CI downloads the difference.

Owner: the release module (`src/release.rs`).

**Exit**: the release's tarball is compressed by a Rust encoder at least as
small as zstd's level 3 on the same tarball, ruzstd's `Default` level once it
is implemented.
