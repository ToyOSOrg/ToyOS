---
status: owner
kind: question
opened: 2026-10-09
---

# Whether pkg alone writes /apps

Two of the owner's rulings in
`issues/a-package-is-a-directory-under-apps-and-the-installer-is-a-program.md`
pull against each other once a package's own directory is read-only to it:

- **"The installer is an ordinary program … with no authority a shell does
  not have"**: `pkg` writes under `/apps` because `/apps` is writable to it.
- **"`/apps/<name>` is immutable by stage-then-commit … nothing writes a
  committed package"** (2026-10-02).

Today every row the image declares, `pkg` and the shell among them, holds
`/apps` read-write (`toyos_manifest::whole_tree`), so a shell and everything
it starts can rewrite an installed package. An installed package itself
holds its own directory read-only (`toyos_manifest::Program::view`).

## The question

Does `pkg` alone write `/apps`, which gives the installer an authority a
shell lacks, or does every row that may start `pkg` keep `/apps` writable,
which leaves a committed package writable by the shell?

## Exit condition

The owner's answer. If `pkg` alone writes `/apps`, every other declared row's
view holds `/apps` read-only, which needs the per-row views of
`issues/every-program-sees-only-the-files-it-was-given.md` stage 2.
