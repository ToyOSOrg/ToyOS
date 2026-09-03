---
status: open
kind: defect
opened: 2026-09-03
---

# The tree pins one container image by digest, runs one lane on a moving tag, and publishes one image no job pulls

`route.yml:131` consumes the T14's image as `127.0.0.1:5000/toyos-ci@sha256:…`,
"a digest and never a moving tag: a rebuild must not be able to change the QEMU
or Rust a recorded number was taken on" (`route.yml:128-130`). That is this
repository's stated rule, and two images are outside it.

`route.yml:123` hands the untrusted lane a bare `debian:sid`, which is a moving
tag on the machine every pull-request verdict is measured on — the same hazard
the digest above exists to refuse, on the lane that renders most of this tree's
verdicts.

`.github/ci-image/Dockerfile`'s image is pushed to `:latest` (`ci-image.yml:43`,
`:47`) and is consumed by nothing — `git grep ci-hosted` returns only
`ci-image.yml:41`, and `ci-image.yml:10-11` says the cutover into `route.yml` is
a separate deliberate act nobody has taken.

**Not closable by pinning, which is why it is not the item that closed beside
it.** `debian:sid` is a rolling distribution and a digest of it is a snapshot
nobody here can rebuild; `.github/ci-image/` builds an image, but the untrusted
lane deliberately does not run a first-party one. So the exit is either the
cutover `ci-image.yml:10-11` describes — the untrusted lane consuming a digest
of an image this repository builds — or a ruling that the untrusted lane's
instrument is allowed to move, written where the lane is chosen.

Narrowed from the action-pinning record this was filed as: every `uses:` in
`.github/workflows/*.yml` is now `owner/repo@<40-hex>` with the tag in a
trailing comment, `CI_ACTIONS` holds those commits, and
`every_action_a_workflow_uses_is_declared` refuses a `uses:` that is not one.
