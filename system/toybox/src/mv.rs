use std::fs;
use std::path::Path;
use std::process;

/// Rename a file within one served filesystem. Cross-volume moves require
/// an explicit copy and delete.
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

    // Report a missing source directly before attempting the rename.
    if let Err(e) = fs::metadata(source) {
        return Err(format!("mv: {}: {e}", source.display()));
    }

    fs::rename(source, &dest).map_err(|e| {
        format!(
            "mv: {} -> {}: {e} — but the source is there, so it is the rename being refused\n\
             mv: most likely the two paths are on different mounts, which is not one rename. \
             Use cp then rm for a move between filesystems.",
            source.display(),
            dest.display()
        )
    })
}
