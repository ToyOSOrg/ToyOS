---
status: open
kind: finding
opened: 2026-08-24
---

# A thread's exit posts `Gone` on its own watch twice, and only the second post is load-bearing

`process::thread_exit` posts a completion before it leaves:

```rust
if let Some(handle) = crate::sched::driver::current_handle() {
    crate::completion::post(
        crate::completion::Subject::of(handle.watch()),
        crate::completion::Outcome::Gone(crate::completion::Reason::Closed),
    );
}
scheduler::exit_current(code);
```

`exit_current` reaches `driver::pass(Dispose::Exit)`, and the pass after that
one drops the task's payload through `Hw::release`
(`kernel/src/hw.rs`), which ends with `TaskHandle::publish_released` —
and that posts `Gone(Closed)` **on the same watch**, which its own comment
states in those words: *"the retirer is armed on this thread's own watch — the
same subject a joiner uses, and the reason the release no longer needs a queue
of its own"* (`kernel/src/sched/payload.rs`).

So a `SYS_THREAD_JOIN` on a sibling is released twice, by two posts to one
subject, and the second one happens whether or not the first is written. The
difference between them is one scheduler pass of latency and nothing else: the
zombie mark that the joiner's predicate reads is already in the table before
either post, because `release_thread` writes it before returning.

**How this was found, and why it is filed rather than acted on.** The host
model of the lifecycle (`toyos-proclife`) was built with a negative control
that reverted the post's subject to the process's main thread — the kernel this
tree had before `1bfe4e5b`, when the wake was by name into a shared parking lot.
The control had no teeth: with `publish_released` modelled faithfully, the
joiner is released in every interleaving anyway. That is a fact about today's
kernel rather than about the model, and it is worth recording because the
opposite is what a reader of `thread_exit` would assume.

**What is owed.** One of two sentences, and the evidence for it:

- the early post buys promptness that a join is entitled to — in which case say
  so at the site, with what a pass of latency costs a joiner; or
- it buys nothing, and it goes, leaving `publish_released` as the one place a
  thread's death reaches its waiters.

Do not answer it by deleting the post and running the suite green: `std_threading`
and every other join in the tree would pass either way, which is the whole
content of this entry.
