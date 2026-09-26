---
status: open
kind: tooling
opened: 2026-09-26
---

# A lane's socket path can pass macOS's 103 bytes

Since #529 a lane's scratch is `$TMPDIR/toyos-tmp-<pid>-0/tests-0/lane-<n>/`,
and on the dev host `$TMPDIR` is `/private/var/folders/gr/<id>/T/`, so the
directory alone is 89 bytes for a four-digit pid and lane 0–9. macOS holds a
Unix socket's path to 103 bytes (`sun_path` is 104 with its NUL), and a
longer one is refused at `bind` ("path must be shorter than SUN_LEN").

A test that meets one reds before its guest boots, on the lane and the pid it
drew rather than on anything it tests: `netd_tcp_neighbour` did, under pid
84151, on `tests/common/segment.rs`'s `tap-out-0.sock` (104 bytes), and the
middlebox's paths were 110. Both have short names now (`tap<n><i|o>.sock`,
`mb<n><t|f><o|i>.sock`, at most 102 bytes in lane 11 under a five-digit pid),
but nothing checks a name against the limit, so the next socket a lane grows
can pass it again. Linux holds one to 107 bytes; whether a CI runner meets
either limit is not measured.

Exit condition: something refuses a lane socket path the dev host cannot bind,
before a guest boots on it.
