---
status: open
kind: tooling
opened: 2026-10-02
---

# A nightly a landing overtakes leaves no release and no SDK alias

The nightly's `release` puts nothing up where main's tip is no longer its
HEAD (`release::sdk_at_tip`), and is green: "main has moved past `<head>`, and
its tip's nightly is the one that publishes". The toolchain release and the
`sdk-<version>` alias then wait for a nightly whose `release` runs at the tip,
or a dispatch there. No landing runs one, the job's conclusion is success
either way, and a nightly overtaken every night publishes nothing until one is
not.

The window runs from the nightly's creation to its `release`'s decision: the
wait in `nightly-<ref>`'s concurrency group, then the `toolchain` job, 2:38 at
its fastest measured (run 36988764929, attempt 2) and 2:11:46 cold (run
36934214557). `cron: '0 3 * * *'` created its scheduled runs at 09:35:54Z on
2026-10-01 (36843762360) and 09:09:27Z on 2026-10-02 (36988155706), and main
took seven landings on 2026-10-02, pushed between 08:39:27Z and 11:20:48Z.
Run 36988155706, at `46af79d5d`, waited behind a dispatched nightly
(36985427800) and started its jobs at 11:45:51Z, four landings after its HEAD
(#643, #664, #679, #659). The release this decision replaced read no tip: a
landing that moved no SDK crate did not stop it.

Owner: the release module (`src/release.rs`) and `nightly.yml`'s `release`.

**Exit**: a landing during a nightly does not leave main's tip without its
toolchain release and SDK alias: after a day on which every nightly was
overtaken, `releases/tags/toolchain-linux-x86_64-<the tip's sysroot key>`
answers 200 and the tip's `sdk-<version>` alias names it.
