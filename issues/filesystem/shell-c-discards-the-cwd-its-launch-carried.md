---
status: open
kind: defect
opened: 2026-09-05
---

# `shell -c` discards the working directory its launch carried

`userland/shell/src/main.rs:27` sets the working directory to `/` before it runs
the line:

```
    if args.len() >= 3 && args[1] == "-c" {
        let input = args[2..].join(" ");
        let _ = env::set_current_dir("/");
        execute_line(&input);
```

A launch carries a working directory — `toyos::launch::Launch::cwd`, which
`/system/bin/init` applies with `Command::current_dir` before it spawns — so a
caller that asked for one is overruled, and every relative path in the line
resolves against `/` instead. The interactive path has the same shape one line
down, where `set_current_dir(&home)` is at least a stated policy.

## Reproduction

Spawn `/system/bin/shell` with `Command::current_dir("/home/root")` and the
arguments `-c`, `pwd`. It answers `/`.

Read from the source rather than measured: `pkg_install_gbae`'s
`relative-path` probe is written around this behaviour (its dotted paths are
rooted at `/` for that reason) and so does not isolate it.

## Exit condition

`shell -c` runs its line in the working directory its launch gave it, and a
guest probe spawns it with a `current_dir` and reads `pwd` back.
