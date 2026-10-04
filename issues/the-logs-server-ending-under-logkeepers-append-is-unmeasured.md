---
status: open
kind: tooling
opened: 2026-09-27
---

# The log's server ending under logd's append is unmeasured

`fsd_restart` ends only DATA's server. The log's is restarted by the same row,
and logd holds its file open across it: a handle reopened after an end is
checked against the identity it last saw, and an append the server ended
under is answered `StaleNetworkFileHandle` and not retried. What logd does
then — whether the lines of that append are lost, repeated, or written after a
reopen — and whether the log file the host reads back after the stop is whole,
has not been run.

**Exit**: a boot that ends the log's server under logd's append (`--end-on` on
a path of the log's volume), and the log file read back off the image by a FAT
implementation that is not fsd's, with what logd said about the lost append.
