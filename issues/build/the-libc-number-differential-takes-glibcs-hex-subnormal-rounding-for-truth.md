---
status: open
kind: tooling
opened: 2026-10-01
---

# The libc number differential takes glibc's hex subnormal rounding for truth

`toyos-libc-copies/src/strtonum_differential.rs` holds libc's `strtod` to the
host C library's, bit for bit. On `ubuntu-24.04` (glibc 2.39, PR #667's `host`
run 36839912041) `random_hexadecimal_numbers_round_as_the_host_s` reds on its
first disagreement:

```
"0X1.c63b83507cf448000P-1025": 4.935062500639595e-309 (0x38c7706a0f9e9), the host's 4.93506250063959e-309 (0x38c7706a0f9e8)
```

The value is `0x1c63b83507cf448000 × 2^-1093`, which is 0x38c7706a0f9e8 plus
0.5625 of the smallest subnormal. That rounds to 0x38c7706a0f9e9, which is
what ours answers and what macOS's libc answers: the test is green on the
development host. glibc's answer is what you get by rounding twice, first to
53 bits (the trailing `8000` is a tie, kept even) and then to the subnormal
(the three bits shifted out, `100`, are a tie, kept even). C11 7.22.1.3p8
requires a correctly rounded result when `FLT_RADIX` is a power of 2. The rest
of the differential agrees with glibc: every other test binary in the host
workspace is green on that run, and so are the decimal, integer and corner
cases.

So on Linux the host is not a valid oracle for hexadecimal input, and the
`host` job is red there until the test judges hexadecimal input by something
else. Two candidates. One is the standard's own requirement, computed exactly
in the test, which is a short big-integer division. The other is macOS libc's
answers for the seeded corpus, captured once and committed, as
`toyos-fat32/tests/fixtures/` captures macOS's FAT tools. The decimal and
integer cases keep the host.

**Exit**: `cargo run -- --ci host` is green on `ubuntu-24.04`, and the
hexadecimal cases still red against a reader that rounds twice.
