---
status: assigned
kind: tooling
opened: 2026-10-02
---

# The guest check gates nothing

`ci.yml`'s `guest` runs the guest suite as `guest / suite` on every non-draft
pull request and every merge group, and main's ruleset does not name it: a
pull request or a merge group whose `guest / suite` is red still lands.
`gh api repos/ToyOSOrg/ToyOS/rulesets/20589156` at 13:59:20Z on 2026-10-02
reads `check_response_timeout_minutes` 240 and one required check, `host`.
Pull request #671, which made the check, says so of itself: "Until step 4 the
guest check gates nothing."

The naming is a ruleset edit, with no commit, and it waits on main. Main's
cache scope holds no toolchain layer (`actions/caches` for `refs/heads/main`
at 14:00:46Z on 2026-10-02: two `host-sealed-` entries and nothing else), so
until `publish.yml`'s `toolchain` has saved the four there, every merge group
builds all four, as pull request run 36934214557 did in 2:11:46, and a check
named before that makes every landing wait on it.

Owner: the orchestrator.

**Exit**: `gh api repos/ToyOSOrg/ToyOS/rulesets/20589156` lists `guest / suite`
among the required checks.
