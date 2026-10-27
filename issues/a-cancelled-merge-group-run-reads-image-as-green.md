---
status: open
kind: defect
opened: 2026-10-27
---

# A cancelled merge-group run reads `image` as green

`.github/workflows/ci.yml`'s `image` job, a required check, has
`if: ${{ github.event_name != 'pull_request' && !cancelled() && needs.scope.outputs.build != 'false' }}`.
`cancelled()` is true when the whole run is cancelled, so then the condition
is false and the job is skipped by it, and GitHub counts a required job its
condition skipped as success ("Using conditions to control job execution").
A cancelled merge-group run then has nothing among its required checks that
reds: `host` runs under `always()` and reds on a part that did not succeed
(#1068), `image` does not.

The `scope` arm needs the same care: a documentation-only group skips `image`
on purpose and must still read green, while a cancelled one must not.

**Exit:** `image` runs, or a job standing for it runs, under `always()` and
reds on a cancelled or failed `toolchain`, while a group `scope` sends past the
build still reads green.
