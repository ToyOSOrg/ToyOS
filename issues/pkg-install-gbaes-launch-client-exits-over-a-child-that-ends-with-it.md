---
status: open
kind: tooling
opened: 2026-10-02
---

# pkg_install_gbae's launch client exits over a child that ends with it

`issues/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md` brings
`pkg_install_gbae` back as a metal row, from `main` before the cut. Its client
there, `pkg_launch_gbae`, launches gbae and exits at once. A launched program
ends with its launcher, so gbae ends as killed before the compositor counts its
window.

Evidence: under QEMU before the cut, `cargo test --test toyos-build --
pkg_install_gbae` exited 1 with the kernel's `exit: gbae pid=21 code=137`
beside the client's own exit, and 0 with the client's launch made
`Command::new(PROGRAM).under_init()` (commit ad21ebf3a).

Exit: the row's client starts gbae under init, and the row is green on the T14.
Owner: the guest-suite track's stage D.
