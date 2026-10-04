---
status: open
kind: tooling
opened: 2026-10-01
---

# The workflow gates that read workflows as text become reviewer instructions or go

Three tests read `.github/workflows/` as text, and each passes a patch that
breaks its rule as GitHub reads the YAML. Each patch, alone, left all three
green (EXIT 0):
- `src/ci.rs`'s `workflows_run_against_main_on_hosted_runners` reads `runs-on:`
  and `pull_request:` off single lines. On `publish.yml`: `runs-on:` with its
  label on the next line, `- self-hosted`; and `pull_request: {branches: [dev]}`
  beside `workflow_dispatch`.
- `src/ci.rs`'s `the_required_check_is_a_job_on_every_pull_request` finds
  ci.yml's triggers and its `host` by substring. On `ci.yml`: `host`'s `if:`
  made `false`. A skipped job reports success, so the required check is green.
- `src/hostws.rs`'s
  `nothing_that_runs_names_a_target_directory_a_member_does_not_have` finds a
  `<member>/target` by substring. On `publish.yml`: a step
  `run: "ls userland/sshd/targe\x74"`, which YAML reads as
  `ls userland/sshd/target`. The same step spelled plainly reds it (EXIT 101).

Owner: `issues/the-tooling-is-a-review-prompt-and-three-workflows.md`.

Done when each test is deleted with its rule a sentence in
`.claude/agents/reviewer.md`, or reds on its patch above.
