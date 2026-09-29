---
status: owner
kind: question
opened: 2026-09-29
---

# Whether doomgeneric becomes a submodule is the owner's

`userland/doom/build.rs` fetches doomgeneric's commit `fc601639` as a GitHub
archive into the gitignored `userland/doom/doomgeneric/` and compiles it. The
fetch costs five build-dependencies in `userland/doom/Cargo.toml` — `ureq`,
`rustls-rustcrypto`, `webpki-roots`, `flate2`, `tar` — and leaves a ToyOS
change to the C nowhere to live but that untracked directory, which the build
replaces whenever its stamp disagrees with the pin. A fork repository of
doomgeneric as a submodule at that path deletes the fetch and the five. Neither
`ToyOSOrg/doomgeneric` nor `Japabu/doomgeneric` exists (`gh repo view`), and
creating one is the owner's.

**Exit**: the owner rules — the fetch stays, or a submodule on a repository he
creates replaces it.
