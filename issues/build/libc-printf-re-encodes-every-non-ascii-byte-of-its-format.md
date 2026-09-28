---
status: open
kind: defect
opened: 2026-09-27
---

# libc's `printf` re-encodes every non-ASCII byte of its format string

`userland/libc/src/printf.rs:269` copies a format's literal bytes with
`w.write_char(fmt[i] as char)`, which turns each byte of a UTF-8 sequence into
the Latin-1 character of the same number and writes that character's UTF-8. So
`printf("привет=%g\n", x)` writes `Ð¿Ñ\u{80}Ð¸Ð²ÐµÑ\u{82}=…`: the bytes are
doubled and wrong. A `%s` argument is not affected.

Found by TinyCC's `83_utf8_in_identifiers`, which toyos-cc never compiled and
clang does; it is declined in `tests/toyos.rs`'s `NOT_RUN` with this entry named.

**Exit**: a format's bytes reach the output unchanged, and
`83_utf8_in_identifiers` runs.
