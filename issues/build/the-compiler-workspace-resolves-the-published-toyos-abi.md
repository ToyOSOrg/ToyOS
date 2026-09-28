---
status: open
kind: defect
opened: 2026-09-28
---
# The compiler workspace resolves the published `toyos-abi` 0.1.0, not the tree's

`rust/Cargo.toml` patches `toyos-abi` to `../toyos-abi`, but a `[patch]` only
replaces a version the requirement accepts. The four forks `rust/Cargo.lock`
pins that name `toyos-abi` — getrandom `toyos-0.2` at 46592414 and `toyos-0.3`
at d3045447, libloading `toyos` at fa0abe77, stacker `toyos` at c25842ac — each
require `"0.1"`, and the tree's crate is 0.16.0. So every bootstrap run
re-locks `rust/Cargo.lock` onto `toyos-abi 0.1.0` from crates.io and records
`[[patch.unused]] toyos-abi 0.16.0`; the committed lock still names the path
package from when the tree's crate was 0.1.0. On a macOS or Linux host those
dependencies are `cfg(target_os = "toyos")` and never compiled; the
ToyOS-hosted rustc (`x86_64-unknown-toyos`) compiles them, so its next build
links getrandom, libloading and stacker against the published 0.1.0 ABI —
predicted from the re-locked file, not built.

getrandom and libloading are `issues/build/the-toolchain-pins-an-older-commit-of-three-userland-forks.md`;
stacker has no branch with the SDK range.

**Owner**: the toolchain re-lock that closes the linked issue, with stacker's
requirement widened on its branch too; it moves every compiler's key, so it is
scheduled, not folded into a build-tooling change.
**Exit**: `rust/Cargo.lock` as a bootstrap run leaves it names no registry
`toyos-abi` and no `[[patch.unused]]`.
