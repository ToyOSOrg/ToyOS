---
status: open
kind: defect
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

The container images are the contrast, and they are why this is a gap and not a
convention: `.github/ci-image/Dockerfile` and `.github/runner/Dockerfile` are
both **consumed pinned by digest** exactly so "a rebuild cannot change what a
verdict was measured with" (`.github/ci-image/Dockerfile:1-8`). The same
argument applies to an action and is not applied.

## What it costs to close

Rewrite each `uses:` as `owner/repo@<40-hex>` with the tag in a trailing
comment, and make `CI_ACTIONS` hold the digest rather than the tag — the shape
`COMMITTED_FILES` already is, where a row and the bytes cannot drift apart. The
price is that a security release of `actions/checkout` no longer arrives by
itself; somebody moves the digest, which is the point.

Not done here because it is a supply-chain decision about six third-party
dependencies rather than a scan, and the ledger that makes it checkable had to
exist first.
