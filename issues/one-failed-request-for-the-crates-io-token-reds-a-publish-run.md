---
status: open
kind: tooling
opened: 2026-10-03
---

# One failed request for the crates.io token reds a publish run

`publish.yml`'s `rust-lang/crates-io-auth-action` step trades the job's OIDC
token for a crates.io token in one request, and the job ends where that
request fails. Run 36998451606, `main` at `7c4a648b3`: the step logged
`Requesting token from: https://crates.io/api/v1/trusted_publishing/tokens`
and, 89 ms later, `##[error]fetch failed`; `cargo run -- --ci publish` was
skipped, and the run was over 12 s after it was created. Nothing ran it
again. The next landing's run, 37000495781 at `c1c504835`, was green, and
36998451606 is the one red among the forty `publish` runs on `main` from
36772021165 to 37110586853.

Until the next landing, what that tip changed in the SDK crates is not on
crates.io, and the nightly's `release` refuses such a tip
(`issues/a-release-that-decides-before-its-tips-crates-are-up-reds-the-nightly.md`).

Owner: the orchestrator.

**Exit**: a publish run whose first request for the token fails asks again,
within a bound, before it reds.
