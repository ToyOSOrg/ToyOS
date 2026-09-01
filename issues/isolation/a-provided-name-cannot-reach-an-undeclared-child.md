---
status: open
kind: defect
opened: 2026-08-10
---

# A transferred connector cannot be merged into an inherited namespace

`Command::provide(name, connector)` says "give the child this connector under
this name, on top of whatever else it holds". The launcher can do that: init
builds the child's namespace itself, from the manifest row plus the extras the
caller transferred (`userland/init/src/main.rs`, `build_namespace`).

The **direct** path still cannot, and the reason is no longer the ABI.

**The ABI half is closed.** `NamespaceBuild::flags` carries
`NAMESPACE_KEEP_ALL` (`toyos-abi/src/syscall.rs`), `Builder::keep_all` is its
SDK spelling (`toyos/src/namespace.rs`), `sys_namespace_build` carries the
base's whole entry set when the bit is set and refuses a bit it does not define
(`kernel/src/arch/syscall/ipc.rs`), and `endowment_denied`'s
`the_base_plus_one_more_name` asserts both halves in a guest.

**What is left is the std fork, and only the std lane can do it.**
`rust/library/std/src/sys/process/toyos.rs`'s `spawn` endows the parent's
namespace handle unchanged — `inherited_namespace` duplicates it and pushes it
under `SVC_LABEL` — so a caller that transferred a connector to a program the
manifest does **not** declare, the one case where the launcher answers
`MSG_NOT_DECLARED` and the SDK falls back to the direct path, still hands the
child a name it cannot resolve. Neither side is told.

It is unreached today only by coincidence: every extras-carrying caller in the
tree (the terminal and the console, both transferring `surface` to
`/bin/shell`) happens to go through the launcher, and the shell's own children
inherit `surface` because init merged it in. That is the kind of safety that
ends the moment somebody adds a caller.

## What closing it takes

`spawn`'s direct path building `keep_all(parent) + add(extras)` and endowing
the result as `svc`, in place of the bare duplicate. That is one function in the
rust submodule, which needs an exclusive machine window, so it is owed to the
std lane and to nothing else.

The instrument is a guest arm: a binary the manifest does not declare, spawned
with a `provide`d connector by a caller that holds a `launcher` connector, must
resolve both that name and everything it inherited — and the same spawn without
the extra must still resolve the inherited set alone, so the arm is not passing
because the child was given a fresh namespace.
