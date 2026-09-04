---
status: owner
kind: question
opened: 2026-09-04
---

# Five package decisions are the owner's, each with the orchestrator's recommendation

`issues/apps/a-package-is-a-directory-under-apps-and-the-installer-is-a-program.md`
assumes the recommendation in each. A one-line answer closes this file and
moves into that one.

1. **Binary-first or source-first.** Install the release's built archive
   (recommended: it is what gbae publishes, and building on ToyOS waits for
   hosted rustc), or fetch source and build on the machine.
2. **The trust chain's first form.** HTTPS plus the release's `SHA256SUMS`
   (recommended, because it exists today), with signatures as stage 4, or
   signatures before anything installs.
3. **Consent.** Ask at install (recommended: the moment the user typed the
   command) or at first run.
4. **gbae's CI on ToyOS.** Should the gbae repository run its binary in a
   ToyOS guest in CI, using the harness's boot image as an artifact this
   repository publishes (recommended later, after stage 1 proves the run),
   or stay build-and-link only.
5. **Sequencing.** Stage 1 starts after root filesystem PR 2b lands
   (recommended: `/apps` exists then) and before the storage track's users
   and mount-protocol stages, which do not block it.
