---
status: open
kind: tooling
opened: 2026-10-27
---

# A cancelled merge-group run reads `image` as green

`.github/workflows/ci.yml`'s `image` job, a required check, has
`if: ${{ github.event_name != 'pull_request' && !cancelled() && needs.scope.outputs.build != 'false' }}`.
`cancelled()` is true when the whole run is cancelled, so then the condition
is false and the job is skipped by it, and GitHub counts a required job its
condition skipped as success ("Using conditions to control job execution").
So a merge-group run whose four host parts have all succeeded, cancelled
while `toolchain` runs, passes both of its required checks: `host` is green,
because every part it needs concluded success, and `image` is skipped by its
own condition, which reads as green.

The `scope` arm needs the same care: a documentation-only group skips `image`
on purpose and must still read green, while a cancelled one must not.

**Exit:** `image` runs, or a job standing for it runs, under `always()` and
reds on a cancelled or failed `toolchain`, while a group `scope` sends past the
build still reads green.
