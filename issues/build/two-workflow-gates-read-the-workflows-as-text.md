---
status: open
kind: tooling
opened: 2026-10-01
---

# Two workflow gates read the workflows as text

`src/ci.rs`'s `workflows_run_against_main_on_hosted_runners` reads `runs-on:`
and `pull_request:` off single lines, and
`the_required_check_is_a_job_on_every_pull_request` finds ci.yml's triggers and
its `host` by substring. A spelling YAML reads the same way passes them. Each
patch below, alone on `publish.yml`, left both tests green (EXIT 0):
- `runs-on:` with its label on the next line, `- self-hosted`;
- `pull_request: {branches: [dev]}` beside `workflow_dispatch`.

Done when both read `src/workflow.rs`'s structure.
