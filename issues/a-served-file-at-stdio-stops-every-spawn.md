---
status: open
kind: defect
opened: 2026-10-21
---

# A served file at stdio stops every spawn

A file a file server holds is a descriptor naming an object of
`toyos::files`', not a kernel handle, and `toyos::fd::copy` refuses one for a
child (`EOPNOTSUPP`, std's `Unsupported`). libc's `posix_spawn` asks for
descriptors 0 to 2 on every spawn (`spawnreq::plan`), so once a C program has
put a served file there, by `dup2` or `freopen`, every `posix_spawn`,
`system` and `popen` it makes is refused. So is an `adddup2` of a served
descriptor and an `addopen` of a served path, and std's `Command` with
inherited stdio in the same case. Before served files were descriptors, no
served file could sit at 0 to 2. Read from the code; no guest case has run it.

**Exit**: a served file can be given to a child, as PR 4 of the descriptor
work gives served kinds (`EXPORT`), and a guest C case redirects stdout to a
file under `/home`, runs `system`, and reads the child's output back from it.
