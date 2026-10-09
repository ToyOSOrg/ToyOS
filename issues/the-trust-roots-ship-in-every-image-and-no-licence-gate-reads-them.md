---
status: open
kind: tooling
opened: 2026-10-09
---

# The trust roots ship in every image and no licence gate reads them

`build::trust_roots` (`src/build.rs`) writes the Mozilla root program's
authorities, as the `webpki-root-certs` crate publishes them, to every ROOT as
`/system/etc/ssl/cert.pem`. That crate's licence is `CDLA-Permissive-2.0`,
which `licence::ALLOWED` does not name, and the gate (`src/licence.rs`) never
sees it: it judges the crates an image's programs link and the files the tree
commits, and the roots are neither — a dependency of the build system whose
data the build copies into the image. The image carries no copy of that
licence's text.

**Exit**: the licence gate judges data the build writes into an image from a
third-party crate, and the roots file passes it: under a licence `ALLOWED`
names, or an `EXCEPTIONS` row, with whatever notice its licence asks of a
redistribution in the image.

**Owner**: the internet-clients track
(`issues/the-internet-clients-work-unchanged.md`), whose stage 3 put the file
there.
