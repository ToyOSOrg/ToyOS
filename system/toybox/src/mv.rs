use std::fs;
use std::path::Path;
use std::process;

/// Move a file, which here is exactly one `SYS_RENAME` and nothing else.
///
/// There is no copy-and-delete fallback for a move between mounts, and the
/// reason is not that the pieces are missing — `cp` is right there. It is that
/// `sys_rename` in `kernel/src/syscall/fs.rs` collapses all five of
/// `Vfs::rename`'s errors into `NotFound`, and `Stat` carries no mount
/// identity, so this process cannot tell "different mounts" from "the rename
/// is broken". A fallback keyed on *any* rename failure would quietly copy its
/// way past a rename defect and pass every test that a working rename passes.
/// So the refusal names both paths, says what it could not distinguish, and
/// stops.
pub fn main(args: Vec<String>) {
    let [source, dest] = args.as_slice() else {
        eprintln!("Usage: mv <source> <dest>");
        process::exit(1);
    };
    if let Err(refusal) = move_file(Path::new(source), Path::new(dest)) {
        eprintln!("{refusal}");
        process::exit(1);
    }
}

/// `source` renamed onto where `dest` lands it ([`crate::cp::destination`]),
/// or the refusal, said as `mv` says it.
pub fn move_file(source: &Path, dest: &Path) -> Result<(), String> {
    let dest = crate::cp::destination(source, dest);

    // Asked before the rename so the commonest cause gets its own answer
    // rather than the one the kernel returns for everything.
    if let Err(e) = fs::metadata(source) {
        return Err(format!("mv: {}: {e}", source.display()));
    }

    fs::rename(source, &dest).map_err(|e| {
        format!(
            "mv: {} -> {}: {e} — but the source is there, so it is the rename being refused\n\
             mv: most likely the two paths are on different mounts, which is not one rename. \
             SYS_RENAME reports one error for every cause, so mv cannot tell that from a rename \
             defect and will not copy behind your back — use cp then rm.",
            source.display(),
            dest.display()
        )
    })
}
