---
status: open
kind: defect
opened: 2026-10-11
---

# The kernel resolves a path by rules of its own

A path on the kernel's own mounts, ROOT and `/tmp`, is resolved by
`kernel/src/vfs.rs`, and not as POSIX resolves one:

- `..` is read off the text before any name is looked up (`normalize`), so
  `/tmp/d/dir/../f`, where `dir` links to `/tmp/x`, names `/tmp/d/f` and not
  `/tmp/f`.
- Only the last name's link is followed (`resolve_for_open`): a path
  through a link to a directory, `/tmp/d/dir/f`, names nothing.
- A relative link is read against the mount it is on, the first name of its
  path, and not against its own directory.
- Only `open` follows the last name's link: `list` and `cd` follow none, so
  `opendir` and `stat` of a link to a directory refuse what `open` and
  `realpath` reach.
- A chain past `toyos_abi::syscall::MAX_LINKS` is refused `InvalidArgument`,
  which libc's `open` answers `EINVAL`; POSIX's answer is `ELOOP`, which libc's
  `realpath` gives.

A file server resolves what it is handed POSIX's way, with a bound of its own
(`system/fileserver/src/resolve.rs`, `MAX_LINKS`), so the same link reads
differently on `/tmp` and on a served directory. It is handed no `..` of an
absolute or working-directory path, which the client reads off the text first
(`issues/a-working-directory-path-reads-its-dot-dot-off-the-text.md`). libc's `realpath`
(`sdk/libc/src/resolve.rs`, `resolve_path`) resolves the names a file server
holds POSIX's way and hands the kernel the rest at the first name it holds,
answering that part by the kernel's rules, so that a program that
canonicalises a path and then opens it opens the same file;
`sdk/libc/arch/src/path_resolution.rs` pins both.

**Owner**: the kernel VFS, `kernel/src/vfs.rs`.

**Exit**: a guest test opens `/tmp` files through a link before the last name,
through a `..` after a link, and through a relative link read against its own
directory, each getting POSIX's file, and is refused `ELOOP` past the bound;
and `path_resolution.rs`'s `dot_dot_is_read_off_the_text_before_any_name_is_looked_up`,
`a_link_before_the_last_name_is_not_followed` and
`a_relative_link_is_read_against_its_mount_not_its_directory` are inverted
with `resolve.rs` in the same change.
