---
status: assigned
kind: tooling
opened: 2026-11-04
---

# The nightly is frozen, so nothing runs the guest matrix or the aarch64 rows a PR does not run

The owner froze the nightly on 2026-11-04 and disabled `nightly.yml` on
GitHub. Root `CLAUDE.md` says the broad integration matrix runs nightly and
that nightly covers the aarch64 guest rows a PR does not run; while it is
frozen, neither happens. Its other jobs stop with it: the host cache's
writer, main's toolchain release, and the fresh-machine and macOS builds.

Guest rows run only where an agent dispatches them on the guest runner
(`guest-rows.yml`), so a regression in a row no PR selected goes unseen.

`build/ci/qemu-version` moved to 11.1.3, the guest runner's and the dev host's
QEMU, while `guest.yml`'s container still installs 11.1.1 from its snapshot
archive, so its instrument step reds until the container moves to an image
carrying 11.1.3.

**Exit**: the nightly resumes, its guest container on 11.1.3. Held by the owner.
