//! `cargo run -- --pr` and `cargo run -- --sync` — the landing protocol after
//! it moved to GitHub.
//!
//! Landing used to be `--land`: an integration lock on this host, `git merge
//! --no-ff main`, the whole suite as a gate, `git -C <primary> merge --ff-only`.
//! The gate ran on the dev host, which is arm64 cross-arch TCG, and there is a
//! class of defect that machine cannot execute at
//! all — 64 boots of 64 lost on an AMD host while every run here stayed green.
//! So the gate moved to twelve KVM shards on x86_64 and the merge moved with it.
//!
//! What did *not* move is the property the gate rests on. `--land` merged main
//! into the branch and gated the **merged result**, which is what catches a
//! semantic conflict between two branches that each pass alone. GitHub's native
//! merge queue is the feature that provides that, and since 2026-08-20 it is
//! **on**: the repository moved to the `ToyOSOrg` organization (the rule type
//! is org-only — measured on the personal account as `Validation Failed:
//! Invalid rule 'merge_queue'`, which is the history the strict-substitute era
//! below came from), and `main`'s ruleset carries a required `merge_queue`
//! rule. The queue builds each merge's exact composition and runs the required
//! checks on it before `main` moves; `gh pr merge --auto --merge` enqueues.
//!
//! Before the organization existed, the substitute was a **strict** required
//! status check — branches up to date before merging — which bought the same
//! property by serialising landings: the first merge moved main and every
//! other branch was stale until it merged again. That tax, its measured
//! breach, and the eased-law interlude between the two regimes are the
//! tracker's record (`the-eased-merge-law-carries-a-threshold`); `--pr`
//! remains the local half either way.
//!
//! Three rules gate a branch here and in CI's `abi-split` job alike: the
//! ABI-first rule ([`abi_lands_alone`]), the writing law
//! (`crate::writinglaw`) and the published crates (`crate::sdkversion`).
//!
//! Nothing here rewrites history and nothing pushes `main`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::buildlock;

/// The trailer that declares a sysroot change which genuinely cannot be landed
/// on its own — an ABI item the branch renames or removes, whose old form the
/// rest of the tree still uses.
///
/// It was `--land --abi-inseparable`, a flag whose only record was the landing
/// commit `--land` wrote. A trailer is in the branch's own history, lands with
/// it, and is the same word to the local check and to the one in CI — which a
/// flag could never be, because CI has no command line from the author.
const ABI_INSEPARABLE: &str = "Abi-Inseparable:";

/// The flag that says the caller will re-run the gates on the merged shape.
const ACCEPTS_MERGE: &str = "--gates-after-merge";

/// What `--pr` says last, and exits non-zero on, when it merged `origin/main`
/// in: every gate the agent ran before this ran on a tree that no longer
/// exists. The push succeeded — the exit is the re-run owed, and a
/// `--pr && gh pr create` chain stopping here is the point of it.
const MERGED_SHAPE: &str = "[pr] pushed; origin/main was merged in, so this exits 1 until the \
                            merged shape is gated: re-run cargo test --lib, --clippy and \
                            --build-only, then run --pr again (it will merge nothing and exit \
                            0) or pass --gates-after-merge.";

/// Whether this run owes a re-run: it merged, and nothing on the command line
/// says the caller will gate the shape the merge made.
fn owes_a_rerun(merged: bool, args: &[String]) -> bool {
    merged && !args.iter().any(|a| a == ACCEPTS_MERGE)
}

pub fn dispatch_pr(root: &Path, args: &[String]) {
    match prepare(root) {
        Err(refusal) => {
            eprintln!("{refusal}");
            std::process::exit(1);
        }
        Ok(done) => {
            println!("{}", done.text);
            if done.merged {
                println!("{MERGED_SHAPE}");
            }
            if owes_a_rerun(done.merged, args) {
                std::process::exit(1);
            }
        }
    }
}

pub fn dispatch_sync(root: &Path) {
    report(sync(root).map(|line| format!("[sync] {line}")));
}

/// The ABI-first rule as a check something other than a human can run.
pub fn dispatch_abi_check(root: &Path, args: &[String]) {
    let base = base_arg(args);
    report(abi_lands_alone(root, &base).map(|()| {
        format!("[abi] this branch's commits against {base} do not mix the shared sysroot's \
                 sources with work that depends on them.")
    }));
}

/// The writing law as the same kind of check; `crate::writinglaw` is the rule.
pub fn dispatch_writing_law_check(root: &Path, args: &[String]) {
    let base = base_arg(args);
    report(crate::writinglaw::judge(root, &base).map(|line| format!("[prose] {line}")));
}

/// The `--base <ref>` both checks take, because CI has no local `main`: a
/// pull-request checkout of the head branch knows `origin/main` and nothing
/// else.
fn base_arg(args: &[String]) -> String {
    args.iter()
        .position(|a| a == "--base")
        .map_or("origin/main", |pos| {
            args.get(pos + 1).map_or("origin/main", String::as_str)
        })
        .to_string()
}

/// `--land` is retired, and it answers rather than going missing.
///
/// A command that vanishes produces `no such subcommand` at an agent working
/// from a brief written last week. This is the same words the brief should have
/// had, delivered where the agent is actually looking.
pub fn dispatch_retired_land() {
    eprintln!(
        "[land] `--land` is retired. `main` moves through pull requests and CI now, and this \
         command moved main on this host from a gate that ran on it.\n\
         [land] The dev host is arm64 cross-arch TCG and cannot execute the class of defect \
         that lost 64 boots of 64 on an AMD KVM host, so the gate is twelve KVM \
         shards on x86_64 instead.\n\
         [land]\n\
         [land]   cargo run -- --pr      merge origin/main into this branch, push it, and print \
         the `gh` command that opens the pull request\n\
         [land]   cargo run -- --sync    fast-forward this machine's `main` to origin/main\n\
         [land]\n\
         [land] CLAUDE.md's workflow section is the protocol."
    );
    std::process::exit(1);
}

fn report(outcome: Result<String, String>) {
    match outcome {
        Ok(text) => println!("{text}"),
        Err(refusal) => {
            eprintln!("{refusal}");
            std::process::exit(1);
        }
    }
}

/// Everything a branch needs before GitHub will look at it.
///
/// The order is the one that costs least when it refuses: the local questions
/// first, then one fetch, then the merge, then the push.
fn prepare(root: &Path) -> Result<Prepared, String> {
    let branch = preflight(root)?;
    let mut lines = vec![sync(root)?];

    abi_lands_alone(root, "origin/main")?;
    lines.push(crate::writinglaw::judge(root, "origin/main")?);
    lines.push(crate::sdkversion::judge(root, "origin/main")?);

    let (merged, line) = merge_base_into_branch(root, &branch)?;
    lines.push(line);

    let carried = git(root, &["rev-list", "--count", "origin/main..HEAD"])?;
    if carried.trim() == "0" {
        return Err(format!(
            "[pr] {branch} has nothing origin/main does not already have, so there is nothing to \
             open a pull request for."
        ));
    }

    // Asked of the remote and asked *before* the push, because it is the one
    // question here that stops being answerable a second later.
    let first_push = git(root, &["ls-remote", "--heads", "origin", &branch])
        .is_ok_and(|refs| refs.trim().is_empty());

    git(root, &["push", "-u", "origin", &branch]).map_err(|e| {
        format!(
            "{e}\n\
             [pr] the push was refused. Nothing here forces one — a pushed branch is a hash \
             somebody's CI run may already have cited — so if this is a non-fast-forward, the \
             remote has commits this worktree does not."
        )
    })?;
    lines.push(format!("pushed {branch} to origin"));

    let head = git(root, &["rev-parse", "--short", "HEAD"])?;
    Ok(Prepared {
        merged,
        text: format!(
            "[pr] {}\n\
             [pr] {branch} is at {head} on origin.\n\
             [pr]\n\
             {}\n\
             [pr] main must be *in* this branch for the merge button to unlock, which is what \
             the merge above is for. If main moves again, re-run `cargo run -- --pr`.",
            lines.join("\n[pr] "),
            if first_push { open_it_now(&branch) } else { finish_it(&branch) },
        ),
    })
}

/// What `--pr` did: `merged` is whether this run brought `origin/main` into the
/// branch, which is what the exit code turns on.
struct Prepared {
    merged: bool,
    text: String,
}

/// **The first push is where the draft belongs, and a branch's first `--pr` is
/// the only moment anyone is reading this.**
///
/// Nothing runs CI on a branch push — deliberately, since
/// a push and the pull request on it were two runs of the same twelve shards. So
/// a branch without a pull request is a branch nothing has ever gated, and that
/// is not a corner case: eleven agents took `wt/toyos-endow` to completion with
/// zero CI exposure, which is how a `rust` submodule pin that matched in twelve
/// hex digits survived four green local suites.
///
/// A draft costs nothing and buys a run on every push. It used to be four words
/// in a parenthesis at the end of the line that recommended `--fill`.
fn open_it_now(branch: &str) -> String {
    format!(
        "[pr] **Open it as a draft now, on this first chunk.** CI runs on a pull request and \
         on nothing else, so until one exists this branch is ungated however long it lives.\n\
         [pr]\n\
         [pr]   gh pr create --draft --base main --head {branch} \\\n\
         [pr]       --title \"{branch}: in progress\" --body \"opened early; CI on every push\"\n\
         [pr]\n\
         [pr] Then push as often as you like. `cargo run -- --pr` again when it is finished."
    )
}

/// What to run on every later `--pr`, which is a branch that already has a
/// remote and probably a draft.
///
/// **Never `--fill`.** It composes the title and body out of the commits, and
/// those two become the merge commit's — main's record of what landed, written
/// on purpose rather than concatenated.
fn finish_it(branch: &str) -> String {
    format!(
        "[pr]   gh pr ready   (if it is a draft and it is finished)\n\
         [pr]   gh pr edit --title \"<what landed>\" --body-file <file>\n\
         [pr]   gh pr merge --auto --merge   (merges when the required checks pass)\n\
         [pr]   gh pr checks --watch   (what the gate is doing)\n\
         [pr]\n\
         [pr] If {branch} has no pull request yet, open one as a draft first:\n\
         [pr]   gh pr create --draft --base main --head {branch} --title \"<what landed>\"\n\
         [pr]\n\
         [pr] The title and body become the merge commit's, so write them as main's record. \
         Not `--fill`: that concatenates the commits."
    )
}

/// The refusals that do not need the network.
fn preflight(root: &Path) -> Result<String, String> {
    let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch == "main" {
        return Err("[pr] this worktree is on main, so there is nothing to open a pull request \
                    for. `cargo run -- --worktree add <path>` makes one to work in."
            .to_string());
    }
    if branch == "HEAD" {
        return Err("[pr] this worktree is on a detached HEAD; a pull request needs a branch."
            .to_string());
    }
    if merging(root) {
        return Err(format!(
            "[pr] a merge of main into {branch} is still unresolved here.\n\
             [pr] resolve it, `git add` the files, `git commit`, then re-run \
             `cargo run -- --pr`."
        ));
    }
    let dirty = git(root, &["status", "--porcelain"])?;
    if !dirty.is_empty() {
        return Err(format!(
            "[pr] this worktree has uncommitted work, and CI would gate a tree main is not going \
             to get:\n{dirty}\n\
             [pr] commit it — on your own branch that is free — then re-run \
             `cargo run -- --pr`."
        ));
    }
    Ok(branch)
}

/// `git fetch origin`, then this machine's `main` onto `origin/main`.
///
/// **`origin/main` is the truth now and the local one is a cache.** `--land`
/// moved the primary's `main` itself, as its own step 4; nothing does that any
/// more, so without this the primary's tree — which owns `rust/`, the sysroot
/// and the witness every worktree compares against — silently falls behind
/// whatever GitHub merged.
///
/// Under the integration lock, which is what is left of its old job: one process
/// at a time moves this host's `main`, and the primary is a checkout somebody
/// may be building in.
///
/// It is housekeeping and not a gate, so a primary that is dirty or on another
/// branch is *reported*, not refused. `--land` had to refuse — it was about to
/// fast-forward that tree onto the branch being landed — and a pull request is
/// not.
fn sync(root: &Path) -> Result<String, String> {
    let primary = crate::primary_checkout(root);
    let _lock = buildlock::integration(root);

    git(root, &["fetch", "--quiet", "origin", "main"])
        .map_err(|e| format!("{e}\n[pr] `git fetch origin main` failed, so nothing below could \
                              be judged against what GitHub has."))?;

    // The fast-forward runs on whatever branch the primary has out, so the
    // question is about that checkout and not about which checkout is asking.
    let on = git(&primary, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if on != "main" {
        return Ok(format!(
            "fetched origin; {} is on {on}, so this host's main was left where it is",
            primary.display()
        ));
    }

    if canonical(root) != canonical(&primary) {
        let dirty = git(&primary, &["status", "--porcelain"])?;
        if !dirty.is_empty() {
            return Ok(format!(
                "fetched origin; {} has uncommitted work in it, so this host's main was left \
                 where it is",
                primary.display()
            ));
        }
    }

    let before = git(&primary, &["rev-parse", "--short", "main"])?;
    let behind = git(&primary, &["rev-list", "--count", "main..origin/main"])?;
    if behind.trim() == "0" {
        return Ok(format!(
            "fetched origin; this host's main is current at {before}{}",
            reclaimable(root)
        ));
    }
    git(&primary, &["merge", "--ff-only", "origin/main"]).map_err(|_| stranded(&primary))?;
    let after = git(&primary, &["rev-parse", "--short", "main"])?;
    Ok(format!(
        "fetched origin; this host's main {before} -> {after} ({} commit(s)){}",
        behind.trim(),
        reclaimable(root),
    ))
}

/// What this host could give back, said where it becomes true.
///
/// A worktree whose branch has landed has no reason to hold its build caches,
/// and `--sync` runs at exactly the moment that becomes true of one.
fn reclaimable(root: &Path) -> String {
    crate::worktree::reclaim_line(&crate::worktree::survey(root, false))
        .map_or_else(String::new, |line| format!("\n[pr] {line}"))
}

/// This host's `main` has commits GitHub does not, so it is not a cache of
/// `origin/main` any more and nothing can fast-forward it.
///
/// **It happened within minutes of `main` being protected**, and by exactly the
/// route this names: another worktree's `--land`, built before that command was
/// retired, fast-forwarded the primary onto its own branch and could then never
/// push it. Eleven commits, all of them still on the branch. So the refusal
/// lists what is stranded and says where to look for it, because "settle that"
/// on its own sends an agent to read the reflog.
fn stranded(primary: &Path) -> String {
    let extra = git(primary, &["log", "--oneline", "origin/main..main"]).unwrap_or_else(|e| e);
    let holders = git(primary, &["branch", "--contains", "main", "--list", "wt/*"])
        .unwrap_or_else(|e| e);
    format!(
        "[pr] this host's main carries commits origin/main has not got, so it cannot be \
         fast-forwarded and it is no longer a copy of what GitHub has. Nothing pushes main any \
         more, so these arrived from a landing that predates that:\n{extra}\n\
         [pr] branches that already contain all of them:\n{}\n\
         [pr] If one of those holds every commit above, nothing is lost — open a pull request \
         for it and put this host's main back with \
         `git -C {} reset --hard origin/main`. If none does, do not reset anything: work out \
         where those commits live first.",
        if holders.trim().is_empty() { "[pr]     none".to_string() } else { holders },
        primary.display(),
    )
}

/// The merged-result property, made by hand because no merge queue will make it.
///
/// A conflict is left in the working tree rather than aborted: the index git has
/// already built and the markers it has already written are exactly what the
/// agent resolves against, and an abort deletes them.
fn merge_base_into_branch(root: &Path, branch: &str) -> Result<(bool, String), String> {
    let base = git(root, &["rev-parse", "--short", "origin/main"])?;
    match git(root, &["merge", "--no-ff", "--no-commit", "origin/main"]) {
        Ok(_) if !merging(root) => Ok((false, format!("origin/main {base} is already in {branch}"))),
        Ok(_) => {
            let message = format!(
                "{branch}: merged main {base} before the pull request\n\n\
                 The required checks are strict, so GitHub refuses the merge button until this \
                 branch contains main — which also makes the checks that run on this head checks \
                 on the merged result.\n"
            );
            let file = root.join(format!("target/pr-merge-{}.txt", std::process::id()));
            fs::create_dir_all(root.join("target"))
                .map_err(|e| format!("[pr] create {}/target: {e}", root.display()))?;
            fs::write(&file, &message)
                .map_err(|e| format!("[pr] write {}: {e}", file.display()))?;
            git(root, &["commit", "-q", "-F", &file.to_string_lossy()])?;
            let _ = fs::remove_file(&file);
            Ok((true, format!("merged origin/main {base} into {branch}")))
        }
        Err(e) if merging(root) => Err(format!(
            "{e}\n\
             [pr] merging main into {branch} conflicts. The merge is left in this worktree, not \
             aborted: its index and its markers are what you resolve against.\n\
             [pr] resolve, `git add`, `git commit`, then re-run `cargo run -- --pr`.\n\
             [pr] nothing was pushed."
        )),
        Err(e) => Err(format!("[pr] `git merge --no-ff origin/main` failed:\n{e}")),
    }
}

/// **A sysroot change may not be landed with work that depends on it.**
///
/// `toyos-abi/src`, `toyos/src` and `userland/libc/src` are compiled into the
/// one sysroot every worktree on this host builds against, so a branch that
/// changes them holds it from the moment it builds until the moment it lands.
/// The rule every brief carries — land the sysroot half on its own first — makes
/// that window one landing instead of one task. **It was followed once in four
/// times on 2026-08-07.** Two agents lost about 35 and about 50 minutes to one
/// miss, and two landings burned a full build each against the refusal in
/// `toolchain.rs`.
///
/// It refused at the cause in `--land`, and CI is now the only thing between a
/// branch and main, so it refuses there. The same function answers both, which
/// is the point: an agent that runs `--pr` gets the refusal in a second and a
/// branch that skipped `--pr` gets it from the `abi-split` check.
///
/// Merges are skipped: a branch's own update merges touch nothing of their own,
/// and counting them as unrelated work would refuse a branch whose only commit
/// is the ABI change.
///
/// **This reads history, not the tree — so the split is decided at the first
/// commit, not fixed later.** A branch that will touch `toyos-abi/src`,
/// `toyos/src` or `userland/libc/src` puts that change in its first commit or
/// on its own branch from the start; a later revert leaves the commit in
/// history and the only remedy is rebuilding the branch. Cost one full rebuild
/// on 2026-08-19.
///
/// **The ABI half is not always a pure declaration.** A syscall that gains a
/// capability argument gains it in the ABI PR with the kernel accepting and
/// ignoring it — the wrapper's signature and the kernel's argument decoding are
/// one fact and cannot land apart — and the demand that *reads* it is the
/// second PR. `SYS_SYSINFO`'s roster right landed that way on 2026-08-22.
pub fn abi_lands_alone(root: &Path, base: &str) -> Result<(), String> {
    let commits = branch_commits(root, base)?;
    if commits.iter().any(|c| c.declares_inseparable) {
        return Ok(());
    }
    let (touching, rest): (Vec<_>, Vec<_>) = commits.iter().partition(|c| c.touches_sysroot);
    if touching.is_empty() || rest.is_empty() {
        return Ok(());
    }

    let listed = |commits: &[&Commit]| {
        commits
            .iter()
            .map(|c| format!("[abi]     {}  {}", c.sha, c.subject))
            .collect::<Vec<_>>()
            .join("\n")
    };
    // A prefix is the only shape the remedy below is available for: git can put
    // the first N commits on main by themselves and cannot put a later one there
    // without the ones under it, and nothing in this workflow rebases.
    let last_touching = commits.iter().rposition(|c| c.touches_sysroot).expect("non-empty");
    let remedy = if commits[..=last_touching].iter().all(|c| c.touches_sysroot) {
        format!(
            "[abi] Land the sysroot half by itself first — the shortest claim window there is:\n\
             [abi]     git switch -c {}-abi {}\n\
             [abi]     cargo run -- --pr        # and merge that pull request\n\
             [abi]     git switch - && cargo run -- --pr",
            git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|_| "HEAD".into()),
            commits[last_touching].sha,
        )
    } else {
        "[abi] The sysroot commits are not the oldest ones here, so they cannot be landed by \
         themselves without reordering — and nothing in this workflow rebases. Either redo the \
         split on a fresh branch, or declare it below."
            .to_string()
    };

    Err(format!(
        "[abi] this branch changes the shared sysroot's sources and carries {} commit(s) that do \
         not.\n\
         [abi] {} are compiled into the one sysroot every worktree builds against, so this \
         branch holds it from its first build until it lands. Landing the sysroot half on its \
         own makes that window one landing instead of one whole task — followed once in four \
         times on 2026-08-07, and each miss cost two agents 35 to 50 minutes.\n\
         [abi]   touching the sysroot's sources:\n{}\n\
         [abi]   the rest:\n{}\n\
         {remedy}\n\
         [abi] If the two genuinely cannot be split — an ABI item this branch renames or removes, \
         whose old form the rest of the tree still uses — put a `{ABI_INSEPARABLE} <why>` trailer \
         in one of this branch's commit messages. It lands with the branch and stays as the \
         record.\n\
         {}\
         [abi] Nothing was pushed and main was not touched.",
        rest.len(),
        crate::toolchain::SYSROOT_SOURCES.join(", "),
        listed(&touching),
        listed(&rest),
        if commits.iter().any(|c| c.bare_inseparable) {
            format!(
                "[abi] A `{ABI_INSEPARABLE}` with nothing after the colon is in this branch's \
                 history and it declares nothing: the why is the whole of the escape.\n"
            )
        } else {
            String::new()
        },
    ))
}

struct Commit {
    sha: String,
    subject: String,
    touches_sysroot: bool,
    declares_inseparable: bool,
    /// The trailer with nothing after the colon: tracked so the refusal can say
    /// why it did not take.
    bare_inseparable: bool,
}

/// `Abi-Inseparable: <why>` on `line`, and whether the why is there. The reason
/// is the contract — CLAUDE.md declares "the split that genuinely cannot be
/// made" — so the keyword alone is a word typed, not a declaration. "There"
/// means one non-whitespace byte; a reason that says nothing is a reviewer's.
fn inseparable(line: &str) -> Option<bool> {
    line.trim_start().strip_prefix(ABI_INSEPARABLE).map(|why| !why.trim().is_empty())
}

/// `<base>..HEAD` oldest first, merges excluded.
///
/// **Two calls, because one log cannot say which of its lines is a path.** Only
/// the first line of `%b` can carry a sentinel, so under `--name-only` a message
/// quoting `toyos-abi/src/…` at the start of a line was read as a changed file.
fn branch_commits(root: &Path, base: &str) -> Result<Vec<Commit>, String> {
    let range = format!("{base}..HEAD");
    let messages =
        git(root, &["log", "--reverse", "--no-merges", "--format=\x01%h %s%n%b", &range])?;
    let paths = git(
        root,
        &["log", "--reverse", "--no-merges", "--name-only", "--format=\x01%h", &range],
    )?;

    let mut commits = parse_messages(&messages);
    for (sha, path) in parse_paths(&paths) {
        if let Some(commit) = commits.iter_mut().find(|c| c.sha == sha) {
            commit.touches_sysroot |= in_sysroot(&path);
        }
    }
    Ok(commits)
}

/// Headers and message text, from a log that asked for no paths — so no line here is one.
fn parse_messages(out: &str) -> Vec<Commit> {
    let mut commits: Vec<Commit> = Vec::new();
    for line in out.lines() {
        if let Some(header) = line.strip_prefix('\x01') {
            let (sha, subject) = header.split_once(' ').unwrap_or((header, ""));
            commits.push(Commit {
                sha: sha.to_string(),
                subject: subject.to_string(),
                touches_sysroot: false,
                declares_inseparable: false,
                bare_inseparable: false,
            });
            continue;
        }
        let Some(last) = commits.last_mut() else { continue };
        match inseparable(line) {
            Some(true) => last.declares_inseparable = true,
            Some(false) => last.bare_inseparable = true,
            None => {}
        }
    }
    commits
}

/// Each changed path against the commit that changed it, from a log whose format carries no message.
fn parse_paths(out: &str) -> Vec<(String, String)> {
    let mut sha = String::new();
    let mut changed = Vec::new();
    for line in out.lines() {
        match line.strip_prefix('\x01') {
            Some(header) => sha = header.to_string(),
            None if !line.trim().is_empty() => changed.push((sha.clone(), line.to_string())),
            None => {}
        }
    }
    changed
}

/// A path inside one of the sysroot's trees; the prefix ends at a directory boundary, so `toyos-abi/srcs` is not `toyos-abi/src`.
fn in_sysroot(path: &str) -> bool {
    crate::toolchain::SYSROOT_SOURCES
        .iter()
        .any(|tree| path.strip_prefix(tree).is_some_and(|rest| rest.starts_with('/')))
}

fn merging(root: &Path) -> bool {
    git(root, &["rev-parse", "--verify", "--quiet", "MERGE_HEAD"]).is_ok()
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|e| panic!("canonicalise {}: {e}", path.display()))
}

/// `Err` carries what git printed, both streams, because a refusal that hides
/// git's own message makes the agent run the command again by hand to see it.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("run git in {}: {e}", dir.display()));
    let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    if out.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim_end().to_string();
    Err(format!("git {} (in {})\n{stdout}\n{stderr}", args.join(" "), dir.display()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A bare "origin" with a `main`, and a clone of it on a branch — the only
    /// shape `--pr` runs in. Signing off and an identity on each repository:
    /// the host's global config signs every commit, and a test that waited on
    /// gpg would be a test that hangs. `writinglaw`'s tests stage in it too.
    pub(crate) fn repo(name: &str) -> (PathBuf, PathBuf) {
        let pid = std::process::id();
        let origin = std::env::temp_dir().join(format!("toyos-pr-{name}-{pid}.git"));
        let work = std::env::temp_dir().join(format!("toyos-pr-{name}-{pid}"));
        let _ = fs::remove_dir_all(&origin);
        let _ = fs::remove_dir_all(&work);

        let seed = std::env::temp_dir().join(format!("toyos-pr-{name}-{pid}-seed"));
        let _ = fs::remove_dir_all(&seed);
        fs::create_dir_all(&seed).unwrap();
        sh(&seed, &["init", "-q", "-b", "main"]);
        identify(&seed);
        fs::write(seed.join("f"), "base\n").unwrap();
        fs::write(seed.join(".gitignore"), "target/\n").unwrap();
        sh(&seed, &["add", "f", ".gitignore"]);
        sh(&seed, &["commit", "-qm", "base"]);
        sh(&seed, &["clone", "-q", "--bare", ".", origin.to_str().unwrap()]);
        // The remote records every value a ref takes, which is the only way a
        // test on this side can see the *order* a push happened in.
        sh(&origin, &["config", "core.logAllRefUpdates", "true"]);

        sh(&std::env::temp_dir(), &["clone", "-q", origin.to_str().unwrap(), work.to_str().unwrap()]);
        identify(&work);
        sh(&work, &["switch", "-q", "-c", "wt"]);
        (origin, work)
    }

    fn identify(dir: &Path) {
        sh(dir, &["config", "user.email", "t@t"]);
        sh(dir, &["config", "user.name", "t"]);
        sh(dir, &["config", "commit.gpgsign", "false"]);
    }

    pub(crate) fn sh(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("run git")
            .success();
        assert!(ok, "git {args:?} in {}", dir.display());
    }

    pub(crate) fn commit(dir: &Path, file: &str, text: &str, msg: &str) {
        if let Some(parent) = Path::new(file).parent() {
            fs::create_dir_all(dir.join(parent)).unwrap();
        }
        fs::write(dir.join(file), text).unwrap();
        sh(dir, &["add", file]);
        sh(dir, &["commit", "-qm", msg]);
    }

    /// **The rule that was refused at the cause in `--land` and now has to be
    /// refused by CI too**, so both ends ask the same function the same
    /// question. Followed once in four times on 2026-08-07.
    #[test]
    fn a_branch_mixing_the_sysroot_with_dependent_work_is_refused() {
        let (_origin, wt) = repo("abi-mixed");
        commit(&wt, "toyos-abi/src/lib.rs", "pub struct A(pub u64);\n", "abi: widen A");
        commit(&wt, "g", "mine\n", "vfs: the work that depends on it");

        let refusal = abi_lands_alone(&wt, "origin/main")
            .expect_err("an ABI change carrying unrelated work must be refused");
        assert!(refusal.contains("shared sysroot's sources"), "{refusal}");
        assert!(refusal.contains("abi: widen A"), "the ABI commit is not named:\n{refusal}");
        assert!(refusal.contains("vfs: the work"), "the rest is not named:\n{refusal}");
        assert!(refusal.contains("git switch -c wt-abi"), "no remedy for a prefix:\n{refusal}");
        assert!(refusal.contains(ABI_INSEPARABLE), "{refusal}");
    }

    /// **A commit message is not a list of paths.** Only the first line of `%b`
    /// can carry a sentinel, so a body wrapping a sysroot path to the start of a
    /// line was counted as a change to that tree, and a branch was refused for a
    /// split it had not made.
    #[test]
    fn a_sysroot_path_quoted_in_a_commit_body_is_not_a_changed_file() {
        let (_origin, wt) = repo("abi-quoted");
        fs::write(wt.join("note.md"), "a record\n").unwrap();
        sh(&wt, &["add", "note.md"]);
        sh(&wt, &[
            "commit",
            "-qm",
            "tracker: the reboot needs an ABI word\n\n\
             It is blocked on a new syscall number in\n\
             toyos-abi/src/syscall.rs and on the wrapper beside it, since\n\
             toyos-abi/src toyos/src userland/libc/src carry no reboot today.",
        ]);

        let listed = git(&wt, &[
            "log",
            "--reverse",
            "--no-merges",
            "--name-only",
            "--format=\x01%h",
            "origin/main..HEAD",
        ])
        .expect("list the branch's paths");
        let changed: Vec<String> = parse_paths(&listed).into_iter().map(|(_, p)| p).collect();
        assert_eq!(changed, ["note.md"], "the paths call saw something that is not a path");

        let commits = branch_commits(&wt, "origin/main").expect("read the branch");
        assert_eq!(commits.len(), 1);
        assert!(
            !commits[0].touches_sysroot,
            "a body quoting {:?} was read as a change to one",
            crate::toolchain::SYSROOT_SOURCES,
        );

        commit(&wt, "g", "mine\n", "vfs: work that depends on none of it");
        abi_lands_alone(&wt, "origin/main")
            .expect("a markdown commit beside a code commit is not a sysroot split");
    }

    /// The prefix ends at a directory boundary, so a sibling is not the tree.
    #[test]
    fn a_sysroot_prefix_matches_the_tree_and_not_its_siblings() {
        assert!(in_sysroot("toyos-abi/src/syscall.rs"));
        assert!(in_sysroot("userland/libc/src/stdio.c"));
        assert!(!in_sysroot("toyos-abi/srcs/syscall.rs"));
        assert!(!in_sysroot("toyos-abi/src"));
        assert!(!in_sysroot("toyos-acpi/src/fadt.rs"));
    }

    /// The escape is a trailer rather than a flag, because CI has no command
    /// line from the author and a flag's only record was a commit `--land`
    /// wrote — and it is the reason that declares, so the keyword alone is not
    /// the escape.
    #[test]
    fn the_inseparable_trailer_is_the_escape_and_it_is_in_the_history() {
        let staged = |name: &str, trailer: &str| {
            let (_origin, wt) = repo(name);
            commit(&wt, "toyos-abi/src/lib.rs", "pub struct A(pub u64);\n", "abi: rename A to B");
            fs::write(wt.join("g"), "mine\n").unwrap();
            sh(&wt, &["add", "g"]);
            sh(&wt, &["commit", "-qm", &format!("vfs: every caller of A\n\n{trailer}")]);
            abi_lands_alone(&wt, "origin/main")
        };

        staged(
            "abi-declared",
            "Abi-Inseparable: A's old name is gone, so nothing compiles between the two halves.",
        )
        .expect("a declared branch must pass");

        staged("abi-terse", "Abi-Inseparable: .").expect("one non-whitespace byte is a reason");

        for bare in ["Abi-Inseparable:", "Abi-Inseparable:   "] {
            let refusal = staged("abi-bare", bare)
                .expect_err("a trailer with no reason declares nothing");
            assert!(refusal.contains("declares nothing"), "{refusal}");
            assert!(refusal.contains("shared sysroot's sources"), "{refusal}");
        }
    }

    /// A branch carrying only the sysroot's sources is the shape the rule asks
    /// for, and a branch carrying none of them is every other branch.
    #[test]
    fn an_abi_only_branch_and_an_ordinary_branch_both_pass() {
        let (_origin, wt) = repo("abi-alone");
        commit(&wt, "toyos/src/lib.rs", "pub fn f() {}\n", "sdk: add f");
        abi_lands_alone(&wt, "origin/main").expect("a branch carrying only the sysroot must pass");

        let (_origin, other) = repo("abi-none");
        commit(&other, "kernel/src/lib.rs", "// work\n", "kernel: work");
        abi_lands_alone(&other, "origin/main").expect("a branch touching no sysroot source must pass");
    }

    /// The whole point of `--pr`: main is *in* the branch before GitHub is asked
    /// to merge it, so the checks that run are checks on the merged result.
    ///
    /// **Read off the remote, because the merge helper cannot say it**: a
    /// `prepare` that pushed, merged and pushed again leaves the same end
    /// state, and only the sequence of values the branch ref took tells them
    /// apart. One landing is staged and one push asserted, so the main read
    /// back is the main that push owed.
    #[test]
    fn the_branch_gets_main_before_it_is_pushed() {
        let (origin, wt) = repo("merge-first");
        commit(&wt, "g", "mine\n", "work");

        // Someone else lands while this branch is being prepared.
        let theirs =
            std::env::temp_dir().join(format!("toyos-pr-merge-first-theirs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&theirs);
        sh(&std::env::temp_dir(), &["clone", "-q", origin.to_str().unwrap(), theirs.to_str().unwrap()]);
        identify(&theirs);
        commit(&theirs, "h", "theirs\n", "meanwhile");
        sh(&theirs, &["push", "-q", "origin", "main"]);

        let done = prepare(&wt).expect("--pr should merge origin/main in and push");
        assert!(done.merged, "{}", done.text);
        assert!(done.text.contains("merged origin/main"), "{}", done.text);
        assert!(fs::read_to_string(wt.join("h")).is_ok(), "main's file did not arrive");
        let subject = git(&wt, &["log", "-1", "--format=%s"]).unwrap();
        assert!(subject.starts_with("wt: merged main"), "{subject}");
        assert!(
            !subject.contains("Merge branch 'main' into"),
            "git's own direction is still what the history records: {subject}"
        );

        let main = git(&origin, &["rev-parse", "refs/heads/main"]).unwrap();
        let pushed = git(&origin, &["reflog", "show", "--format=%H", "refs/heads/wt"]).unwrap();
        let pushes: Vec<&str> = pushed.lines().filter(|l| !l.is_empty()).collect();
        for at in &pushes {
            assert!(
                git(&origin, &["merge-base", "--is-ancestor", &main, at]).is_ok(),
                "{at} was pushed without origin/main {main} in it, so CI would have gated a \
                 shape nobody is going to merge:\n{pushed}"
            );
        }
        assert_eq!(pushes.len(), 1, "one push is what a --pr is:\n{pushed}");

        // A second run is a no-op and says so, rather than making an empty
        // merge commit every time an agent re-runs it.
        let again = prepare(&wt).expect("a second --pr must be a no-op");
        assert!(!again.merged, "{}", again.text);
        assert!(again.text.contains("already in wt"), "{}", again.text);
    }

    /// A fixture clone is its own primary, the shape the branch question used
    /// to be skipped for: the fast-forward ran on whatever branch was out, git
    /// refused it, and `sync` reported lost commits about an ancestor.
    #[test]
    fn a_primary_on_a_branch_is_left_where_it_is() {
        let (origin, wt) = repo("sync-on-a-branch");
        commit(&wt, "g", "mine\n", "work");

        // Someone else lands, so this host's main is behind origin/main.
        let theirs = std::env::temp_dir()
            .join(format!("toyos-pr-sync-on-a-branch-theirs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&theirs);
        sh(&std::env::temp_dir(), &["clone", "-q", origin.to_str().unwrap(), theirs.to_str().unwrap()]);
        identify(&theirs);
        commit(&theirs, "h", "theirs\n", "meanwhile");
        sh(&theirs, &["push", "-q", "origin", "main"]);

        let said = sync(&wt).expect("a primary on a branch is reported, never refused");
        assert!(said.contains("is on wt, so this host's main was left where it is"), "{said}");
        assert!(!said.contains("carries commits origin/main has not got"), "{said}");
        // Left where it is, and main still strictly behind.
        assert_eq!(git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(), "wt");
        assert_eq!(git(&wt, &["rev-list", "--count", "main..origin/main"]).unwrap(), "1");
    }

    /// **`--pr` is never the last gate when it merged anything**, and the tool
    /// is the only place that can be said: every gate run before it ran on a
    /// tree that is now history. It exits non-zero unless the caller has said
    /// it will re-run them.
    #[test]
    fn a_pr_that_merged_says_the_gates_ran_on_another_shape() {
        assert!(MERGED_SHAPE.contains("re-run cargo test --lib, --clippy and --build-only"));
        assert!(MERGED_SHAPE.contains(ACCEPTS_MERGE));
        assert!(owes_a_rerun(true, &["--pr".to_string()]));
        assert!(!owes_a_rerun(true, &["--pr".to_string(), ACCEPTS_MERGE.to_string()]));
        assert!(!owes_a_rerun(false, &["--pr".to_string()]));

        let (_origin, wt) = repo("merged-notice");
        commit(&wt, "g", "mine\n", "work");
        let quiet = prepare(&wt).expect("nothing to merge");
        assert!(!quiet.merged, "{}", quiet.text);
        assert!(quiet.text.contains("is already in wt"), "{}", quiet.text);
    }

    /// A conflict is left where the agent can resolve it, and the *next* run
    /// finds it rather than merging over it.
    #[test]
    fn a_conflict_is_left_in_the_worktree_and_recognised_next_time() {
        let (origin, wt) = repo("conflict");
        commit(&wt, "f", "mine\n", "work");

        let theirs =
            std::env::temp_dir().join(format!("toyos-pr-conflict-theirs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&theirs);
        sh(&std::env::temp_dir(), &["clone", "-q", origin.to_str().unwrap(), theirs.to_str().unwrap()]);
        identify(&theirs);
        commit(&theirs, "f", "theirs\n", "meanwhile");
        sh(&theirs, &["push", "-q", "origin", "main"]);

        sh(&wt, &["fetch", "-q", "origin", "main"]);
        let refusal = merge_base_into_branch(&wt, "wt").expect_err("a conflict must refuse");
        assert!(refusal.contains("conflicts"), "{refusal}");
        assert!(merging(&wt), "the conflicted merge was thrown away");
        assert!(
            fs::read_to_string(wt.join("f")).unwrap().contains("<<<<<<<"),
            "the markers to resolve against are gone"
        );
        assert!(preflight(&wt).expect_err("an unresolved merge must refuse").contains("unresolved"));
    }

    #[test]
    fn a_dirty_worktree_and_main_itself_are_refused_by_name() {
        let (_origin, wt) = repo("dirty");
        commit(&wt, "g", "mine\n", "work");
        fs::write(wt.join("g"), "not committed\n").unwrap();
        assert!(preflight(&wt).expect_err("uncommitted work must refuse").contains("uncommitted"));

        sh(&wt, &["checkout", "-q", "--", "g"]);
        sh(&wt, &["switch", "-q", "main"]);
        assert!(preflight(&wt).expect_err("main is not a branch to land").contains("on main"));
    }

    /// **The draft has to be the answer on the push that creates the branch**,
    /// because that is the only moment an agent is reading for what to do next
    /// and CI runs on a pull request and on nothing else.
    ///
    /// `ls-remote` is the question, and the whole point is that it is asked
    /// *before* the push — a second later the answer is the wrong one for ever.
    #[test]
    fn the_first_push_is_told_to_open_a_draft_and_later_ones_are_not() {
        let (_origin, wt) = repo("first-push");
        commit(&wt, "g", "mine\n", "work");

        let before = git(&wt, &["ls-remote", "--heads", "origin", "wt"]).unwrap();
        assert!(before.trim().is_empty(), "the branch is not on the remote yet");
        assert!(open_it_now("wt").contains("gh pr create --draft"));

        let first = prepare(&wt).expect("the first --pr should push and print").text;
        assert!(first.contains("Open it as a draft now"), "{first}");
        assert!(
            !first.contains("create --fill"),
            "--fill composes what becomes the merge commit's message: {first}"
        );

        let after = git(&wt, &["ls-remote", "--heads", "origin", "wt"]).unwrap();
        assert!(!after.trim().is_empty(), "the push happened");

        commit(&wt, "g2", "more\n", "more work");
        let later = prepare(&wt).expect("a later --pr should push and print").text;
        assert!(later.contains("gh pr ready"), "{later}");
        assert!(!later.contains("Open it as a draft now"), "{later}");
        assert!(!later.contains("create --fill"), "{later}");
    }
}
