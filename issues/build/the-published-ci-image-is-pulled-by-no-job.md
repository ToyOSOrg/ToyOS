---
status: open
kind: tooling
opened: 2026-09-21
---

# The one container image this repository publishes is pulled by no job

`ci-image.yml:7-9` states the repository's rule for a container image: a
consumer pins the digest and never the tag, because "a rebuild must not be able
to change the QEMU or Rust a recorded number was taken on".

`.github/ci-image/Dockerfile`'s image is pushed to `:latest` (`ci-image.yml:45`,
`:49`) and is consumed by nothing — `git grep ci-hosted` returns only
`ci-image.yml:43`, and `ci-image.yml:11-13` says the cutover is a separate
deliberate act nobody has taken. What it would buy is the 52-59 s every one of
the thirteen guest jobs spends in `deps`, three times a day —
`issues/build/building-the-image-once-and-shipping-it-cannot-shorten-the-matrix.md`
prices what that is and is not worth.

The exit is the cutover `ci-image.yml:11-13` describes — a guest lane naming a
digest of an image this repository builds, and no `deps` step at all — or
deleting `.github/ci-image/` and `ci-image.yml`, since an artifact published
for nobody is a build nothing reads.

**The other half of this entry is closed.** It filed every guest lane's bare
`debian:sid` as unpinnable, on the reasoning that "a digest of it is a snapshot
nobody here can rebuild". That was wrong twice over: the dated
`debian:sid-YYYYMMDD` tags *are* debuerreotype's build of
`snapshot.debian.org/archive/debian/<date>T000000Z`, so an image and an archive
can be one date; and leaving the image unpinned stopped being a drift and became
an outage, because an image newer than the dated archive carries packages apt
will not downgrade back. Every guest lane and the Dockerfile now name
`debian:sid-<date>@sha256:<digest>` for the date `.github/apt-snapshot`
declares, held there by `src/ci.rs`'s
`every_dated_image_and_the_archive_under_it_name_one_date`.
