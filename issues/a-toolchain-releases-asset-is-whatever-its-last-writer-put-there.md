---
status: open
kind: tooling
opened: 2026-10-01
---

# A toolchain release's asset is whatever its last writer put there

The release that main's publisher puts up is what a consumer outside CI
installs, and so is the SDK alias that names it. The release notes' install
steps take the asset as it is served.

A branch's workflows decide their own token's permissions. A workflow on any
branch of this repository can ask for `contents: write` and then replace that
asset, or create the release for a tag main has not yet published. A pull
request's `toolchain` job held such a token in run 36863809437; its log reads
`Contents: write`. Two nightly runs dispatched on branches built and published
their branches' toolchains under the nightly's write token:
`wt/toyos-castore` published `toolchain-linux-x86_64-688e609acf5a65c4` (run
36709239346), and `wt/toyos-notiers` published
`toolchain-linux-x86_64-92f146618d6687d8` (run 36600425263). Every toolchain release is mutable (`immutable:
false`).

Owner: the release module (`src/release.rs`).

**Exit**: a consumer outside CI installs only the bytes main's publisher
recorded, and its install refuses any other bytes by name. That holds when a
published release can no longer change (with the SDK alias made a new release
per move, never one moved).
