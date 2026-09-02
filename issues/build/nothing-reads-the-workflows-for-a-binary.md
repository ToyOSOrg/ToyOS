---
status: open
kind: defect
opened: 2026-09-01
---

# The binary ledger reads Rust and not the workflows, and CI installs eight packages nothing declares

`src/sourcegate.rs`'s `every_binary_the_host_runs_is_declared` reads every
`Command::new` argument in host Rust and refuses one no row declares. The clause
it half-closes was written over *"every `.rs` file **and every
`.github/workflows/*.yml`**"*, and the second half is not built. Nothing reads a
workflow, a composite action or a container recipe for a binary.

What that misses today, and it is not hypothetical:
`.github/ci-image/Dockerfile` installs `build-essential`, `ca-certificates`,
`curl`, `git`, `jq`, `qemu-system-x86`, `xz-utils` and `zstd` from a pinned
Debian snapshot. Every hosted guest job runs in that image. Two of the eight are
inside the bar and the other six are not declared anywhere a check reads —
`build-essential` is the `cc` standing failure wearing a package name, and
`curl`, `jq`, `xz-utils` and `zstd` are not named by
`issues/build/python-and-cc-are-declared.md` at all.

**The shape, and why it is not the Rust scan again.** A `run:` step is a shell
script, so an enumeration of what it executes is not a parse. What *is* a parse
is the small declarative surface: `uses:` actions by name and pin, and the
package lists a `Dockerfile` or an `apt-get install` line spells out. Those two
carry the eight above and every action the workflows pull, and neither needs to
understand shell. A row per name with a reason, refused when it is not there and
refused when the row goes stale, is the same shape `HOST_SPAWNS` already is.

What it will not reach, stated so nobody reads the closure as larger than it
is: a binary a `run:` step downloads and executes, and a binary a third-party
action runs inside itself. Those are the same class as `cc::Build`'s host `ar`,
which the Rust scan does not reach either and says so.
