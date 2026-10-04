---
status: open
kind: defect
opened: 2026-09-30
---

# libc++ takes its generic locale path on ToyOS

`libcxx/include/__locale_dir/locale_base_api.h` in `ToyOSOrg/llvm-project`
sends ToyOS down its fallback arm, the one it marks temporary: libc++ reaches
the locale through global `_l` names and `bsd_locale_fallbacks.h`, which is why
libc's `locale.rs` carries every `_l` function libc++ calls. Upstream moves
each platform to a header of its own under `__locale_dir/support/`, and a new
platform is asked for one.

ToyOS's is Fuchsia's shape: one C locale, `uselocale` around the calls that
read it, and `support/no_locale/`'s characters and number readers.

**Exit**: `__locale_dir/support/toyos.h`, its `__toyos__` arm in
`locale_base_api.h`, and its entries in `libcxx/include/CMakeLists.txt` and
the module map, in the fork; libc's `_l` functions that only the fallback
called go. It moves the LLVM key, so every host builds LLVM again: it rides
with the next change to the fork's `src/llvm-project`.
