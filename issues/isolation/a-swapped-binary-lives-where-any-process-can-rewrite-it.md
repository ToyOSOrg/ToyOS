---
status: open
kind: defect
opened: 2026-09-23
---

# A swapped binary lives where any process can rewrite it

A service swap (`toyos-swap`) writes the binary init verified to
`/tmp/swap/<sha256>/<service>` and starts the service from that path. `/tmp`
is a tmpfs every process may write, because the filesystem is ambient by the
owner's ruling, so the file init hashed is not the file the kernel loads by any
guarantee — only by nobody having rewritten it in between. Two windows:

- **Between the hash and the spawn.** init reads the staged bytes, verifies
  them and writes them to the installed path itself, so the installed file is
  the verified bytes when it is written. It is spawned by path once the
  requester has hung up, milliseconds to two seconds later; a process that
  rewrites the file in that window runs its own bytes holding the service's
  manifest row — its device claims included.
- **For the service's life.** The kernel pages a binary in on demand, and a
  restore after a failed swap spawns the previous installed path again.

Nothing in the tree does either; the authority to ask for a swap is
`/system/bin/swap`'s alone, and this is the one step of it the capability
model does not cover.

**Exit condition.** The spawn takes what init verified rather than a path: a
spawn from a file handle init holds, or from bytes init hands over — an ABI
change — or a directory only init may write, which the ambient-filesystem
track would have to rule on.

## Narrowed, not closed

init now hashes an installed binary again at every start — the swap's and a
restore's — and refuses to start one that no longer holds what its path names
(`toyos_swap::installed_digest`). That moves the first window from "until the
requester hangs up" to the spawn syscall itself. The second is unchanged: the
kernel still pages the running binary in from the file. A directory only init
may write does not exist in this kernel: every writable mount is ambient, and
gating one on the writer's identity would be pid-as-authority. So the exit
condition above stands, and choosing between its three shapes is the owner's.
