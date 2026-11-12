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

The unfiltered suite now carries the shared boots' guest rows,
`testcases_members_exit_0` and `shared_debug_members_exit_0`, and with them an
in-suite build of every guest binary and corpus case. Their cost on a TCG lane
is unmeasured, and resumption measures it rather than meeting it at a lane's
timeout.

`build/ci/qemu-version` moved to 11.1.3, the guest runner's and the dev host's
QEMU, while `guest.yml`'s container still installs 11.1.1 from its snapshot
archive, so its instrument step reds until the container moves to an image
carrying 11.1.3.

`nightly.yml`'s `portability-linux` job runs in
`public.ecr.aws/docker/library/debian:sid`, a moving tag pulled from the
public internet on every run, so a pull refused for rate is a red: six
nightlies from 2026-10-24 to 10-31 failed that job at `Error response from
daemon: toomanyrequests: Data limit exceeded` before any step ran
(e.g. run 41187139929, job 124496246843). Resumption owes it an image pinned by
digest from a mirror the build controls.

**Exit**: the nightly resumes with its guest lanes on the guest runner rather
than GitHub's machines. Only a `workflow_dispatch`-only workflow may name that
runner, so resumption owes a design for a scheduled job under that guard. Held
by the owner.
