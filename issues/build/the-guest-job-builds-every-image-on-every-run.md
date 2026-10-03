---
status: open
kind: tooling
opened: 2026-10-03
---

# The guest job builds every image on every run

`guest.yml`'s `suite` restores one cache entry, the sysroot, and builds the
rest in the job: the build driver, the harness, and each kernel, loader, ROOT
and test binary a guest boots. Job 111020980149 of run 37061831501 (#649's
head `b6635359a`, a pull request run with the sysroot restored from cache)
ran 18 min 11 s:

- 2 min 46 s in `deps`, before the checkout;
- 2 min 15 s compiling the driver and the harness (`Finished` in 1m 26s and
  in 47.62s);
- 767.5 s in the suite by its own line, of which its 26 `PASS` lines account
  for 208 s. The other 559 s is the log's silence, up to 209 s at a time,
  between a `cleaning` line or a verdict and the next guest's first line:
  three kernel builds by the suite's summary, and the loaders, ROOTs and test
  binaries beside them.

Owner: the orchestrator.

**Exit**: what of those builds an entry restored from `main`'s cache scope
would save is measured on a pull request run.
