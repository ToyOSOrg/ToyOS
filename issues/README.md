# Issues

One file per issue, `issues/<slug>.md`. There is no index and no
numbering: **`ls` is the index and the frontmatter is the query.**

`ls issues/` lists everything. To ask a question of the set:

```
rg -l '^status: open' issues/       # every unheld piece of work
rg -l '^status: assigned' issues/   # what somebody is holding
rg -l '^status: owner' issues/      # what is waiting on the owner
```

## Frontmatter

Four fields, three required, no defaults.

| field | values | means |
|---|---|---|
| `status` | `open` | it is work, and nobody is holding it |
| | `assigned` | it is work, and somebody is — the body says who or which task |
| | `owner` | it is the owner's to decide, and nobody else may |
| | `none` | nothing is owed |
| `kind` | `defect` | real, reproducible, someone should fix it |
| | `tooling` | the development machine — the harness, a gate, a price, CI, the tracker, the build system, a measurement owed |
| | `finding` | noticed in passing — and bounded: when taken up it is promoted to a `defect` or folded into the owning module header and closed |
| | `track` | staged work — something to build that nobody has built |
| | `question` | blocked on the owner, and nobody else can decide it |
| | `rejected` | considered and declined, recorded so nobody re-proposes it |
| `opened` | `YYYY-MM-DD` | the first commit whose issue tracker carried this heading |
| `task` | a number | optional; present only where the issue names one |

**`status` and `kind` are not free of each other.**
`kind` says what the entry is; `status` says what is owed. Two of the kinds
answer that second question by themselves, so they may not contradict it:

| `kind` | `status` must be |
|---|---|
| `defect`, `tooling`, `finding` | `open` or `assigned` |
| `track` | `open` or `assigned` |
| `question` | `owner` |
| `rejected` | `none` |

**`kind: rejected` is not work.** It is here so the next agent does not spend a
day re-deriving an answer the owner already gave. Nothing in a `rejected` file
is owed — and if the body says otherwise, the *kind* is what is wrong. A ruling
that declared a standing failure rather than removing it deferred the work; it
did not decline it, so the entry is a `defect` and stays open.

**`kind: finding` does not accumulate.** A finding
has a bounded life: whoever takes it up either promotes it to a `defect`
(something real that someone should act on — a fix, a measurement, an
instrument) or moves its one durable line to the module header or doc comment
at the site that owns the subject and deletes the file by the closing
procedure below. A fold moves the invariant, never the investigation: one
clause, no dates, no story — the deletion commit carries those. "May never be
worth fixing" is a reason to fold it to the site, never a reason to keep the
file; when unsure, promote — a wrong promotion costs a later demotion, a wrong
fold loses tracked truth.

**`kind: question` is not work either** — not yours. It is owed by the owner,
and an agent that "fixes" one has decided something that was his to decide. But
a file blocked on an *instrument* — a gate, a machine, a measurement — is not a
question. Nobody has to decide it; somebody has to run it.

**`kind: track` is what a plan used to be**, and it is written to the length a
defect is. A `track` says what is to be built, what it is blocked on, and any
constraint a reader would otherwise pay to re-derive — a hardware bound, a
number somebody measured, a design line the owner already drew. It does not
carry a design, a stage table, a rationale or a review history: a design that is
right is written as code, and one that is not yet written is not yet known.

## Slugs

The tracker is one directory with no subdirectories: what an issue is, is its
`kind`, and what it is about, its slug and its body. The **slug** is its
identity — unique across the tracker — so `rg <slug>` finds every pointer at
it. A slug is a claim like any sentence here: one the tree has refuted is
renamed by the work that takes it up, with every citation moved.

## Pointing at one

**Name the file.** `issues/null-sink-applies-one-connect.md` is a claim
something can check; a bare `issues/` says nothing about whether the entry you
meant is still there.

Never write "the entry above" or "the entry below". Position was what the
numbered document had and what this directory exists to be rid of; a positional
reference inside a file that no longer sits beside its neighbour points at
nothing at all.

## Filing one

Write a new file, only where a brief or a review asks for one. A pull request
edits no existing one: a stale or false issue is left as it is, and is updated
only by the work that takes it up.

## Closing one

**Delete the file.** Git keeps the story, and the commit message is where
evidence, measurements and what-the-code-used-to-do belong.

**A close is verified by the review** (`.claude/agents/reviewer.md`), which
reads this file's rules against the branch.

Before you delete it, ask what durable rule it carries — an invariant a future
agent could violate again, independent of the bug that revealed it. One line of
that goes to the module header or the doc comment at the site that owns the
subject, stated as what is true there and citing nothing. The story does not go
with it.

**Every citation outside `issues/` goes in the same merge, so search before you
delete — for the slug as well as the path.** The slug is the identity, and a pointer written as a
bare name is invisible to a path search. Search the *tree* rather than the
checkout (`git grep <rev>`): `rg` skips dotfile directories without `--hidden`,
and `.github/` holds citations too. Then read where the hits are. One in a comment
under `toyos-abi/src`, `toyos/src` or a published crate changes no identity
(`src/identity.rs`), so it owes no version and builds no sysroot.

