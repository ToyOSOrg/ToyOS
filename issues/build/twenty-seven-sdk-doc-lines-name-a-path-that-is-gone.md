---
status: open
kind: tooling
opened: 2026-09-04
---

# Twenty-seven SDK doc lines name `/bin`, which no longer exists

## Reproduction

```
rg -n --pcre2 '(?<![A-Za-z0-9_/.])/(bin|lib|share|etc)(/|(?![A-Za-z0-9_/.-]))' \
   toyos-abi/src toyos/src userland/toyos-window/src
```

Twenty-seven lines, all doc comments: `/bin/init`, `/bin/logd`, `/bin/toybox`,
`/bin/ps`, `/bin/shutdown`, `/bin/sshd`, `/bin/terminal`, `/bin/console`,
`/bin/ls`. ROOT mounts at `/system` since the hierarchy landed, so every one of
them names a path no image carries.

## Why the branch that moved the rest left them

`src/CLAUDE.md`: a doc-comment change under `toyos-abi/src` costs a sysroot
claim exactly like a layout change (`SYSROOT_SOURCES` in `src/toolchain.rs` is
`toyos-abi/src`, `toyos/src`, `userland/libc/src`, and `standing` asks `git
diff` over them). A claim refuses every other checkout on this host until the
claimant lands, and it also makes the claiming branch unable to build its own
base — which is the arm a negative control needs. `userland/toyos-window` is
one of the five crates the ToyOSOrg forks resolve by version off crates.io, so
a change there is a published bump plus every dependent's pin
(`src/sdkversion.rs` refuses the pair).

So the cost of correcting these lines is not the lines: it is a sysroot claim
and an SDK version bump, and neither is worth spending on prose.

## Exit condition

The next branch that claims the sysroot for its own reason deletes them in the
same commit — deleted rather than corrected, per the owner's rule on wrong
prose — and `userland/toyos-window`'s one line goes with the next bump of that
crate. `LOG_DOC_BIN` in `src/build.rs` is the directory the `Rights::LOG` doc
gate strips, and it moves in that same commit or the gate stops seeing a doc
that names the wrong one. This file closes when the `rg` above is empty.
