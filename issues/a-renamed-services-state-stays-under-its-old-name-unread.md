---
status: open
kind: defect
opened: 2026-10-03
---

# A renamed service's state stays under its old name, unread

The supervisor starts a service with `HOME` at `/state/<its name>`
(`toyos-manifest`'s `home`), so a service's state is keyed by its program's
name and a rename orphans it with nothing said. Stage 1 of
`issues/the-supervisor-is-host-tested-and-owns-the-stop.md` renamed
five services, and a DATA volume that outlives that change:

- keeps `/state/logd`, `/state/soundd`, `/state/netd`, `/state/sshd` and
  `/state/blockd`, which nothing reads or deletes;
- answers ssh with a host key `sshserver` mints afresh under
  `/state/sshserver`, which a client that pinned the old key refuses;
- no longer authorizes a key that lived only in `/state/sshd/authorized_keys`:
  `userland/sshserver/src/main.rs`'s `authorized_keys` reads
  `/state/sshserver/authorized_keys` and `/system/etc/ssh_authorized_keys`, so
  a machine updated over ssh whose owner's key was only there refuses its owner
  afterwards.

A package under `/apps` built before the rename asks `toyos::endow::service`
for `soundd` and `netd`, which no row endows, so it finds no sound or network
server until it is reinstalled.

A DATA outlives an image only where one was made to: the boot image carries
none (`src/image.rs`'s `create_boot_image`), and the build stamps the
designation the kernel formats only into a scratch file it has just created
(`src/build.rs`'s `create_sparse`). Every checkout's `target/nvme.img`, which
`cargo run` keeps across runs, is one; a machine holds one only where a stamped
TOYOS-DATA partition was put by hand, and whether any of the owner's does is
not known here.

**Owner**: the orchestrator, which holds the supervisor track above.

**Exit**: the supervisor names at boot every `/state` directory that no service
of the running manifest owns, and a test stages one and reads the line.
