---
status: open
kind: tooling
opened: 2026-10-20
---

# A crate new to the published set reds every publish until someone puts it up by hand

`publish.yml` publishes through crates.io trusted publishing, and a trusted
publishing token cannot create a crate. #905 (2c3f61a3e) put `toyos-untrusted`
and `toyos-transport` into `PUBLISHED` (`build/sdkversion.rs`), std's
dependencies through `library/Cargo.lock`, and neither exists on crates.io. So
main's 27 `publish` runs from 38558545002 (2c3f61a3e, 2026-10-14) to
39497368165 (9b11608ab, 2026-10-20) are red: 25 at the first new crate, and two
stopped earlier, 38719407453 at a git clone and 38727675385 at a moved tip.
The 25 answer

> failed to publish toyos-untrusted ... (status 403 Forbidden): Trusted
> Publishing tokens do not support creating new crates. Publish the crate
> manually, first

The crates after it in `PUBLISHED` have not gone up since: `toyos` and
`toyos-window` were last published on 2026-10-14 at 10:06Z.

The nightly's `release` reads the same plan, finds `toyos-untrusted` owed at
main's tip, and refuses (`release::sdk_at_tip`): "crates.io holds no
toyos-untrusted of this tree, main's tip, so no sdk alias can name it". Runs
38701484274, 38868708235, 39013160165, 39108067326, 39221595037 and
39390441510 are red so, and the newest toolchain release and `sdk-` alias are
nightly 38533374419's, `toolchain-linux-x86_64-sdk-0.71.0` (2026-10-14 08:16Z). The refusal is right here and names the symptom, not the
failed publish (`issues/a-release-that-decides-before-its-tips-crates-are-up-reds-the-nightly.md`).

The publish runs' and the releases' log lines, and crates.io's answers:
https://github.com/ToyOSOrg/evidence/tree/a36ad035c72df67b46d2ccee7a916fe5f521a5a3.

Only an account can create a crate, and only then can it name `publish.yml`
the crate's trusted publisher. Nothing before a landing tells that a crate it
adds to `PUBLISHED` is not on crates.io.

Owner: the orchestrator, for the runs after landing; `build/sdkversion.rs`
for the rule.

Neither crate is owed any more: #965 removed `toyos-untrusted`, and the
transport is now the `toyos` SDK's `transport` module, so `PUBLISHED` names
only crates the crates.io index holds (checked 2026-10-21). Its doc comment
says a crate joins only once crates.io holds it with `publish.yml` its trusted
publisher. Nothing yet shows a green run.

**Exit**: a `publish` run at main's tip after the transport merge lands, and
the next nightly's `release`, are green.
