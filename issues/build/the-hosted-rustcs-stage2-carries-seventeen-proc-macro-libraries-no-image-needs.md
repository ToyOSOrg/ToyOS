---
status: open
kind: defect
opened: 2026-10-03
---

# The hosted rustc's stage2 carries seventeen proc-macro libraries no image needs

The ToyOS-hosted stage2's `lib/` in release
`toolchain-linux-x86_64-48dd24f826263d6c` holds eighteen shared objects:
`librustc_driver`, 486,137,984 bytes, and seventeen proc-macro libraries the
compiler's own build loaded, 179,575,736 bytes together (the listing: #682,
comment 5968053849). The Linux-hosted
stage2 beside it holds `librustc_driver` alone, 594,483,472 bytes, which
`issues/build/toyos-builds-itself.md` measures at 184.7 MB stripped.

`collect_hosted_rustc` (`src/build.rs`) puts every `lib/*.so` of that
directory, and `bin/rustc`, into ROOT as built. `hosted-rustc` is `false` in
`system.toml` and a config that sets it is refused until the compiler's
licences are read (`build::shipped`), so no image carries them today. #629
deletes the function, the key and the refusal, and this paragraph goes in its
merge.

Owner: `issues/build/toyos-builds-itself.md`, M3.

Exit condition: an image that ships the hosted rustc carries `librustc_driver`
stripped and no proc-macro library.
