//! `/system/bin/pkg` — the installer, and an ordinary program.
//!
//! **It holds no authority a shell does not.** It claims no device, receives no
//! service and is endowed nothing; it writes under `/apps` because `/apps` is
//! writable to it. What it does is verify an archive against the release's own
//! `SHA256SUMS`, ask, unpack, and write the `manifest.toml` that records which
//! binary the directory launches.
//!
//! The decisions are `lib.rs`'s and are made on the host; what is left here is
//! the filesystem and the question put to the user.

use std::fs;
use std::io::{BufRead, Write};
use std::path::Path;

use flate2::read::GzDecoder;
use pkg::{archive, sums};
use sha2::{Digest, Sha256};
use toyos_manifest::package::{self, Package};

/// Answers the consent prompt in advance, for a caller with no terminal.
/// Spelled out rather than `-y`: installing without asking says so in full.
const ASSUME_YES: &str = "--yes";

const MAX_INFLATED: u64 = 256 * 1024 * 1024;

/// The release publishes one covering every asset, so it is found by sitting
/// beside the archive rather than by being named on the command line.
const SUMS: &str = "SHA256SUMS";

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match words.as_slice() {
        ["install", file] => install(Path::new(file), false),
        ["install", file, ASSUME_YES] | [ASSUME_YES, "install", file] => {
            install(Path::new(file), true)
        }
        ["remove", name] => remove(name),
        ["list"] => list(),
        _ => Err(format!(
            "usage: pkg install <archive> [{ASSUME_YES}] | pkg remove <name> | pkg list"
        )),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("{why}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn install(file: &Path, assume_yes: bool) -> Result<(), String> {
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("pkg: {} has no file name", file.display()))?
        .to_string();
    let beside = file.parent().unwrap_or(Path::new("/")).join(SUMS);

    // **The sums file first, and its absence is a refusal by name**: verifying
    // only where one happens to be there installs unverified on a typo.
    let sums_text = fs::read_to_string(&beside)
        .map_err(|e| format!("pkg: cannot read {}: {e}", beside.display()))?;
    let want = sums::digest_for(&sums_text, &name)?;

    let bytes = fs::read(file).map_err(|e| format!("pkg: cannot read {}: {e}", file.display()))?;
    let got = sums::hex(&Sha256::digest(&bytes));
    if got != want {
        return Err(format!("pkg: {name} hashes to {got} and {SUMS} says {want}"));
    }
    println!("pkg: verified {name} against {SUMS} ({got})");

    let tar = inflate(&name, &bytes, MAX_INFLATED)?;
    let entries = archive::entries(&tar)?;
    let plan = pkg::plan(&name, &got, &entries)?;
    let package = &plan.manifest;

    let dir = Package::dir(&package.name);
    if fs::metadata(&dir).is_ok() {
        return Err(format!(
            "pkg: {dir} is already there — remove it first, this installer does not replace"
        ));
    }
    if !assume_yes && !consented(package, file)? {
        return Err(format!("pkg: not installing {}", package.name));
    }

    // The manifest is written last, so a directory carrying one is a finished
    // install: init and the launcher both reach a package through it.
    write_package(
        &dir,
        &plan.dirs,
        &plan.files,
        &Package::path(&package.name),
        &package.render()?,
    )?;
    println!(
        "pkg: installed {} {} at {dir}, launching {}",
        package.name, package.version, package.program
    );
    Ok(())
}

/// Inflate `bytes`, refusing an archive that claims more than `ceiling`.
///
/// **The inflated size is the archive's claim and `SHA256SUMS` says nothing
/// about it**, so the reader is bounded rather than the output measured after:
/// measuring is the allocation this refuses.
fn inflate(name: &str, bytes: &[u8], ceiling: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut tar = Vec::new();
    GzDecoder::new(bytes)
        .take(ceiling + 1)
        .read_to_end(&mut tar)
        .map_err(|e| format!("pkg: {name} is not gzip: {e}"))?;
    if tar.len() as u64 > ceiling {
        return Err(format!("pkg: {name} inflates past {ceiling} bytes and is refused unread"));
    }
    Ok(tar)
}

/// Write a package's directory, and take it back down if any write fails: a
/// verified archive can still run out of volume half way, and a directory with
/// no `manifest.toml` is a name `install` refuses for the volume's life.
fn write_package(
    dir: &str,
    dirs: &[String],
    files: &[(String, &[u8])],
    manifest: &str,
    text: &str,
) -> Result<(), String> {
    match write_all(dirs, files, manifest, text) {
        Ok(()) => Ok(()),
        Err(why) => match remove_tree(Path::new(dir)) {
            Ok(()) => Err(format!("{why}; {dir} was taken back down")),
            Err(swept) => Err(format!("{why}; {swept}")),
        },
    }
}

fn write_all(
    dirs: &[String],
    files: &[(String, &[u8])],
    manifest: &str,
    text: &str,
) -> Result<(), String> {
    for dir in dirs {
        fs::create_dir_all(dir).map_err(|e| format!("pkg: cannot create {dir}: {e}"))?;
    }
    for (path, data) in files {
        fs::write(path, data).map_err(|e| format!("pkg: cannot write {path}: {e}"))?;
    }
    fs::write(manifest, text).map_err(|e| format!("pkg: cannot write {manifest}: {e}"))
}

/// The question, asked where the user typed the command. A closed or empty
/// stdin is a "no": installing on silence is what this prompt exists to stop.
fn consented(package: &Package, file: &Path) -> Result<bool, String> {
    print!("Install {} {} from {}? [y/N] ", package.name, package.version, file.display());
    std::io::stdout().flush().map_err(|e| format!("pkg: cannot ask: {e}"))?;
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|e| format!("pkg: cannot read the answer: {e}"))?;
    let answer = answer.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

/// **A directory that answers as no package is still removed**, and said so by
/// name: `/apps/<name>` is what `install` refuses to write over, so a name it
/// cannot take is a name nothing else could free.
fn remove(name: &str) -> Result<(), String> {
    if package::package_of(&format!("{}/x", Package::dir(name))) != Some(name) {
        return Err(format!("pkg: {name:?} is not a package name"));
    }
    let dir = Package::dir(name);
    let manifest = Package::path(name);
    let installed = fs::read_to_string(&manifest).ok().and_then(|t| Package::parse(&t).ok());
    match installed {
        Some(p) if p.name != name => {
            return Err(format!("pkg: {manifest} calls itself {:?}", p.name))
        }
        Some(p) => {
            remove_tree(Path::new(&dir))?;
            println!("pkg: removed {name} {}", p.version);
        }
        None if fs::metadata(&dir).is_ok() => {
            remove_tree(Path::new(&dir))?;
            println!("pkg: removed {dir}, which carried no manifest this installer wrote");
        }
        None => return Err(format!("pkg: {name} is not installed — there is no {dir}")),
    }
    Ok(())
}

/// How deep this walk goes. Policy on the primitive: any process can plant a
/// Delete a directory and everything under it.
///
/// **Not `fs::remove_dir_all`**: that one empties a directory and leaves it
/// (`issues/filesystem/remove-dir-all-empties-a-directory-and-leaves-it.md`),
/// and a package whose directory survives its removal is a name that can never
/// be installed again.
///
/// **An explicit stack and no depth bound.** Recursion put the walk's depth in
/// the gift of whoever wrote under `/apps/<name>`, and a bound only moved the
/// harm: a refusal past it leaves the directory standing, which is the
/// unfreeable name this exists to prevent. What is bounded is memory, by the
/// number of directories rather than by anything a single path can spell.
fn remove_tree(dir: &Path) -> Result<(), String> {
    let mut unvisited = vec![dir.to_path_buf()];
    // Every directory, each one before the ones inside it, so removing in
    // reverse always meets an empty one.
    let mut found: Vec<std::path::PathBuf> = Vec::new();
    while let Some(at) = unvisited.pop() {
        let entries =
            fs::read_dir(&at).map_err(|e| format!("pkg: cannot read {}: {e}", at.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("pkg: cannot read {}: {e}", at.display()))?;
            let path = entry.path();
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                unvisited.push(path);
            } else {
                fs::remove_file(&path)
                    .map_err(|e| format!("pkg: cannot remove {}: {e}", path.display()))?;
            }
        }
        found.push(at);
    }
    for at in found.iter().rev() {
        fs::remove_dir(at).map_err(|e| format!("pkg: cannot remove {}: {e}", at.display()))?;
    }
    Ok(())
}

fn list() -> Result<(), String> {
    let mut names: Vec<String> = Vec::new();
    let dir = toyos_manifest::package::DIR;
    let entries = fs::read_dir(dir).map_err(|e| format!("pkg: cannot read {dir}: {e}"))?;
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            names.push(name.to_string());
        }
    }
    names.sort();
    for name in names {
        match fs::read_to_string(Package::path(&name)).map_err(|e| e.to_string()).and_then(|t| {
            Package::parse(&t)
        }) {
            Ok(p) => println!("{} {} {} {}", p.name, p.version, p.program, p.digest),
            // Named rather than hidden: a directory under `/apps` that is not a
            // package is something the user put there and will want to see.
            Err(why) => println!("{name} — not a package ({why})"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(what: &str) -> std::path::PathBuf {
        let at = std::env::temp_dir().join(format!("pkg-{what}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&at);
        fs::create_dir_all(&at).expect("a scratch directory");
        at
    }

    #[test]
    fn an_install_that_cannot_finish_takes_its_directory_back_down() {
        let root = scratch("unwind");
        let dir = root.join("gbae");
        let dir_s = dir.to_str().unwrap().to_string();
        let good = format!("{dir_s}/gbae");
        // `gbae` is a file, so a write to `gbae/inside` cannot succeed.
        let doomed = format!("{good}/inside");
        let files: Vec<(String, &[u8])> = vec![(good.clone(), b"ELF".as_slice()), (doomed, b"x")];

        let why = write_package(&dir_s, &[dir_s.clone()], &files, "unreached", "unreached")
            .expect_err("the second write cannot succeed");
        assert!(why.contains("was taken back down"), "{why}");
        assert!(!dir.exists(), "{dir_s} survived a failed install");
        assert!(root.exists(), "the unwind went past the package's own directory");
        let _ = fs::remove_dir_all(&root);
    }

    /// No depth a tree can be planted at leaves the name behind: 33 was the
    /// first the old bound refused, and 256 is far past any stack it had.
    #[test]
    fn a_tree_of_any_depth_comes_off_and_leaves_no_directory() {
        for depth in [33usize, 256] {
            let root = scratch(&format!("depth{depth}"));
            let mut at = root.clone();
            for _ in 0..depth {
                at = at.join("d");
            }
            fs::create_dir_all(&at).expect("a deep tree");
            fs::write(at.join("leaf"), b"x").expect("a leaf");
            // A sibling branch as well, so the walk is a tree and not a chain.
            fs::create_dir_all(root.join("wide/er")).expect("a second branch");
            fs::write(root.join("wide/er/leaf"), b"y").expect("a second leaf");

            remove_tree(&root).unwrap_or_else(|e| panic!("depth {depth}: {e}"));
            assert!(!root.exists(), "a tree {depth} deep survived its removal");
        }
    }

    #[test]
    fn an_archive_that_inflates_past_the_ceiling_is_refused_unread() {
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        gz.write_all(&vec![0u8; 64 * 1024]).expect("deflate");
        let bomb = gz.finish().expect("a gzip stream");
        assert!(bomb.len() < 1024, "the fixture is not a bomb: {} bytes", bomb.len());

        let why = inflate("bomb.tar.gz", &bomb, 4096).expect_err("64 KiB past a 4 KiB ceiling");
        assert!(why.contains("inflates past 4096 bytes"), "{why}");
        assert_eq!(inflate("bomb.tar.gz", &bomb, 128 * 1024).expect("under it").len(), 64 * 1024);
        assert!(inflate("not.tar.gz", b"not gzip at all", 4096).is_err());
    }
}
