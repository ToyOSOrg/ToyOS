---
status: open
kind: defect
opened: 2026-09-03
---

# Every guest lane runs on a moving tag, and the one image this repository publishes is pulled by no job

`ci-image.yml:7-9` states the repository's rule for a container image: a
consumer pins the digest and never the tag, because "a rebuild must not be able
to change the QEMU or Rust a recorded number was taken on".

Every guest lane is outside it. `ci.yml:132`, `:333` and `:430`, `gate-a.yml:49`
and `probe-green.yml:31` each name a bare `debian:sid`, which is a moving tag on
the machines every verdict in this tree is measured on.

`.github/ci-image/Dockerfile`'s image is pushed to `:latest` (`ci-image.yml:45`,
`:49`) and is consumed by nothing — `git grep ci-hosted` returns only
`ci-image.yml:43`, and `ci-image.yml:11-13` says the cutover is a separate
deliberate act nobody has taken.

**Not closable by pinning.** `debian:sid` is a rolling distribution and a digest
of it is a snapshot nobody here can rebuild; `.github/ci-image/` builds an
image, but the guest lanes deliberately do not run a first-party one. So the
exit is either the cutover `ci-image.yml:11-13` describes — a guest lane
consuming a digest of an image this repository builds — or a ruling that the
guest lane's instrument is allowed to move, written where the image is named.

Narrowed from the action-pinning record this was filed as: every `uses:` in
`.github/workflows/*.yml` is now `owner/repo@<40-hex>` with the tag in a
trailing comment, `CI_ACTIONS` holds those commits, and
`every_action_a_workflow_uses_is_declared` refuses a `uses:` that is not one.
