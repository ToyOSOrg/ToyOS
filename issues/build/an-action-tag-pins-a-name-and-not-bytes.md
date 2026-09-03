---
status: open
kind: tooling
opened: 2026-09-03
---

# Every `uses:` in this repository is pinned to a moving tag, so the ledger over them declares a name and not code

`src/sourcegate.rs`'s `every_action_a_workflow_uses_is_declared` holds every
`uses:` value in `.github/workflows/*.yml` against `CI_ACTIONS`. That closes the
*set*: an action nobody declared is a red, and a row nothing uses is a red.

It does not close what runs. Every row is a major tag — measured on this tree:

```
$ grep -rn "uses:" .github/workflows/ | grep -c "@v4"
33
$ grep -rn "uses:" .github/workflows/ | grep -v "@v4" | grep -v "\./" | wc -l
       0
```

`actions/checkout@v4` is a git tag the publisher moves whenever it ships a v4
release, so the six declared rows name six repositories and no bytes. A
compromised or simply changed publisher runs different code on the machine every
one of this project's verdicts is measured on, and both this gate and
`.github/instrument.sh` stay green through it — the instrument check reads
QEMU's version, which an action does not touch.

One container image is the contrast, and it is why digest pinning is this
repository's stated rule rather than a preference: `route.yml:131` consumes the
T14's image as `127.0.0.1:5000/toyos-ci@sha256:…`, "a digest and never a moving
tag: a rebuild must not be able to change the QEMU or Rust a recorded number was
taken on" (`route.yml:128-130`). The same argument applies to an action and is
not applied.

**The other two are not a contrast, and an earlier draft of this entry said they
were.** `route.yml:123` hands the untrusted lane a bare `debian:sid`, which is a
moving tag on the machine every pull-request verdict is measured on; and
`.github/ci-image/Dockerfile`'s image is pushed to `:latest` (`ci-image.yml:43`,
`:47`) and consumed by nothing — `git grep ci-hosted` returns only
`ci-image.yml:41`, and `ci-image.yml:10-11` says the cutover into `route.yml` is
a separate deliberate act nobody has taken. So the tree pins one image by
digest, runs one lane on a moving tag, and publishes one image no job pulls.

## What it costs to close

Rewrite each `uses:` as `owner/repo@<40-hex>` with the tag in a trailing
comment, and make `CI_ACTIONS` hold the digest rather than the tag — the shape
`COMMITTED_FILES` already is, where a row and the bytes cannot drift apart. The
price is that a security release of `actions/checkout` no longer arrives by
itself; somebody moves the digest, which is the point.

Not done here because it is a supply-chain decision about six third-party
dependencies rather than a scan, and the ledger that makes it checkable had to
exist first.
