---
status: open
kind: tooling
opened: 2026-09-29
---

# Open and prepared upstream pull requests for the forks, as `forks.toml` held them

`forks.toml` is gone; these facts were held only there, read from
`git show origin/main:forks.toml`.

- **russh** — Eugeny/russh#782 is open (2026-09-26) and is the `rustcrypto`
  cipher backend alone, on the fork's `rustcrypto-backend` branch. The consumed
  `toyos` branch, on v0.60.0, carries a different cipher backend: it is not that
  PR's code. The PR sits on upstream main past v0.63.3 and is rewritten for
  `aes-gcm` 0.11, `chacha20` 0.10 and `poly1305` 0.9 in a new `cipher::rustcrypto`
  module. After a move onto a release that carries it, what is left for ToyOS is
  the `cryptovec` mlock no-ops and `known_hosts_path`, both behind
  `target_os = "toyos"`.
- **wgpu** — prepared, not opened: branch `instance-no-backend-target` on
  `ToyOSOrg/wgpu`, compared against `gfx-rs/wgpu` `trunk`. `Instance::new` gets
  the instance instead of the panic on any target wgpu has no backend for; it
  names no ToyOS. The consumed `toyos-27.0.4` is +10/-3 on v27.0.4.
- **fontdb** — prepared, not opened: branch `add-toyos-system-fonts` on
  `ToyOSOrg/fontdb`, compared against `RazrFalcon/fontdb` `master` (v0.24), +7,
  the Redox arm's shape. `target_os = "toyos"` draws an `unexpected_cfgs`
  warning, which fontdb's CI does not deny.
- **raw-window-handle** — rust-windowing/raw-window-handle#223 is open
  (2026-07-28), branch `add-toyos-support`.
- **target-lexicon** — bytecodealliance/target-lexicon#134 is open
  (2026-07-27), branch `add-toyos-os`.

**Owner**: the fork estate; upstream pull requests are not sent for now (owner).

**Exit**: each open pull request is merged or closed and each prepared branch is
opened or deleted.
