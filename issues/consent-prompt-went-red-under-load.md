---
status: open
kind: defect
opened: 2026-10-11
---

# consent_prompt went red under load

`cargo test`, the whole guest suite, went red on `consent_prompt` in two of
six runs beside 30 CPU burners on a 14-core macOS host, at one-minute load
averages of 79.5 to 90.2 across those two suites, with the same verdict both
times:

```
"`esc` changed nothing on the prompt in 20 s",
"once: the launcher did not open in 20 s",
"the job ended Some(101)",
"the steps finished were [\"always\", \"again\", \"deny\", \"denied\", \"ssh\"], not [\"always\", \"again\", \"deny\", \"denied\", \"skip\", \"once\", \"ssh\"]",
```

Both waits that went red are 20 s of host wall clock that the harness does
not scale (`tests/toyos.rs`, the launcher's open and a key's change of the
prompt). Whether the key was lost or only late is not measured. The two logs
are posted whole on the pull request that filed this file.

Owner: the orchestrator, which dispatches the fix.

**Exit**: the cause is found and fixed, and the fix is shown by whole-suite
runs under at least this load, or the test is deleted with this file
recording the commit that restores it.
