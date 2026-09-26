---
status: open
kind: defect
opened: 2026-09-26
---

# The AArch64 kernel builds with dead code allowed, blanket

`kernel/.cargo/config.toml` gives `aarch64-unknown-none-softfloat`
`-Adead_code -Aunfulfilled_lint_expectations`: until the syscall gate, the
interrupt controller and the scheduler have a way in, every item only they
reach is unreachable on AArch64. The allowance covers the whole kernel, so a
genuinely dead item written anywhere, for either architecture's shared code,
is invisible on this target; only the x86 build, which keeps
`-Dwarnings` whole, still sees it.

Owned by stage 7 of `issues/kernel/toyos-runs-on-arm64.md`, when userland
boots and the gate reaches everything.

**Exit condition**: both `-A` flags are gone from the AArch64 target's
`rustflags` and the AArch64 kernel builds with `-Dwarnings` alone.
