<!--
This body becomes the merge commit's body on `main` and is the reviewer's
evidence: write it as the record of what landed, kept true of the branch as it
stands. `git log --no-merges` is still how you read the work itself.

**Do not hard-wrap the prose.** GitHub wraps this body itself when it composes
the merge commit, and a paragraph you have already wrapped at 78 comes out
ragged at 72 — measured on the first merge through this workflow, `1d43976`.
One line per paragraph; bullets on one line each.

Delete anything below that has nothing to say. An empty heading is worse than
no heading.
-->

## What this changes


## Why


## Evidence
<!--
Each gate with its exit code. Numbers come from a command that was actually
run, with the run id or the command. An estimate says it is one. -->


## What is unsure


## Anything a reader of `main` must not miss
<!--
A test it deletes and the issue recording the commit that restores it, an
`issues/` file it closes or invalidates.
-->
