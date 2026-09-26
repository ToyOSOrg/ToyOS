---
status: owner
kind: question
opened: 2026-09-26
---

# `doom.jpg` shows id's art, and nothing records the terms it is under

`doom.jpg`, the screenshot `README.md` shows, is a picture of this system
running doom, so most of its pixels are `assets/DOOM1.WAD`'s graphics.
Its row in `COMMITTED_FILES` (`src/licence.rs`) declares `NOASSERTION`,
because the tree's `MIT OR Apache-2.0` does not reach id's art, and the
shareware terms in `NOTICE` do not name screenshots. No image
ships the file, so the licence gate does not judge it.

**Exit**: the owner rules on the terms, and the row says them. Or the
screenshot is replaced by one that shows only this tree's own work.
