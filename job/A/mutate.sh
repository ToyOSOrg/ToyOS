#!/bin/bash
# mutate.sh: each mutation applied to the committed tree, saved as a checked patch, its test run, the tree restored.
set -u
WT=/home/user/toyos-rootless; M=/root/.claude/jobs/015tYBoMwh9xUcBBLG35wTFr/A/mutations
cd "$WT"
run() { # name file sed-expr package filter
  local name=$1 file=$2 expr=$3 pkg=$4 filter=$5
  sed -i "$expr" "$file"
  git diff > "$M/$name.patch"
  if [ ! -s "$M/$name.patch" ]; then echo "$name: SED DID NOT APPLY"; exit 3; fi
  git apply --check -R "$M/$name.patch" || { echo "$name: patch does not check"; exit 3; }
  cargo test -p "$pkg" $( [ "$pkg" = toyos-build ] && echo --lib ) -- "$filter" > "$M/$name.log" 2>&1
  local code=$?
  git checkout -- "$file"
  echo "$name: EXIT=$code ($(grep -c '^error' "$M/$name.log") compile errors) $(grep -E '^test .*(FAILED|ok)$' "$M/$name.log" | tr '\n' ' ')"
}
run m1-stamp-write-ignored src/build.rs 's|    fs::write(&stamp, identity.to_string()).unwrap_or_else(.e. panic!("write {}: {e}", stamp.display()));|    let _ = fs::write(\&stamp, identity.to_string());|' toyos-build build::tests::a_stamp_that_cannot_be_written_panics
run m2-record-before-removal src/sysroot.rs 's|^    let record = build_dir.join("compiled-by");$|    let record = build_dir.join("compiled-by");\n    fs::write(\&record, identity).unwrap();|' toyos-build sysroot::tests::a_switch_that_cannot_remove
run m3-unreadable-dir-skipped src/toolchain.rs 's|    for entry in fs::read_dir(dir).unwrap_or_else(.e. panic!("read {}: {e}", dir.display())) {|    for entry in fs::read_dir(dir).into_iter().flatten() {|' toyos-build toolchain::tests::dep_info_that_cannot_be_read_is_refused
run m4-stuck-left-in-root toyos-tmpdir/src/lib.rs 's|            stuck(tmp, &dir, e);|            if false { stuck(tmp, \&dir, e) }|' toyos-tmpdir a_directory_the_sweep_cannot_remove
run m5-content-unchecked src/llvm.rs 's|    (placed != now).then|    (placed != now \&\& false).then|' toyos-build llvm::tests::a_placed_llvm_is_read_only
run m6-file-bytes-unhashed src/llvm.rs 's|std::io::copy(&mut file, &mut hasher)|std::io::copy(\&mut { let _ = \&file; std::io::empty() }, \&mut hasher)|' toyos-build llvm::tests::a_placed_llvm_is_read_only
run m7-not-read-only src/llvm.rs 's|^    read_only(&partial);$|    if false { read_only(\&partial) }|' toyos-build llvm::tests::a_placed_llvm_is_read_only
git status --porcelain --ignore-submodules=none
