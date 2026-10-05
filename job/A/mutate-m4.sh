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
run m4-stuck-left-in-root toyos-tmpdir/src/lib.rs 's|            stuck(tmp, &dir, e);|            if false { stuck(tmp, \&dir, e) }|' toyos-tmpdir a_directory_the_sweep_cannot_remove
git status --porcelain --ignore-submodules=none
