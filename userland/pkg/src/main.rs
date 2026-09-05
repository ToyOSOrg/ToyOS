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
use toyos_manifest::package::Package;

/// Answers the consent prompt in advance, for a caller with no terminal.
/// Spelled out rather than `-y`: installing without asking says so in full.
const ASSUME_YES: &str = "--yes";

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

    let mut tar = Vec::new();
    std::io::Read::read_to_end(&mut GzDecoder::new(&bytes[..]), &mut tar)
        .map_err(|e| format!("pkg: {name} is not gzip: {e}"))?;
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

    for dir in &plan.dirs {
        fs::create_dir_all(dir).map_err(|e| format!("pkg: cannot create {dir}: {e}"))?;
    }
    for (path, data) in &plan.files {
        fs::write(path, data).map_err(|e| format!("pkg: cannot write {path}: {e}"))?;
    }
    // **Last, so a directory carrying one is a finished install**: init and the
    // launcher both reach a package through its manifest.
    let manifest = Package::path(&package.name);
    fs::write(&manifest, package.render()?)
        .map_err(|e| format!("pkg: cannot write {manifest}: {e}"))?;
    println!(
        "pkg: installed {} {} at {dir}, launching {}",
        package.name, package.version, package.program
    );
    Ok(())
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

/// Removing a package is deleting its directory — and only a directory that
/// answers as a package, so a mistyped name deletes nothing.
fn remove(name: &str) -> Result<(), String> {
    let manifest = Package::path(name);
    let text = fs::read_to_string(&manifest)
        .map_err(|e| format!("pkg: {name} is not installed ({manifest}: {e})"))?;
    let package = Package::parse(&text)?;
    if package.name != name {
        return Err(format!("pkg: {manifest} calls itself {:?}", package.name));
    }
    let dir = Package::dir(name);
    remove_tree(Path::new(&dir))?;
    println!("pkg: removed {name} {}", package.version);
    Ok(())
}

/// Delete a directory and everything under it, depth first.
///
/// **Not `fs::remove_dir_all`**: that one empties a directory and leaves it
/// (`issues/filesystem/remove-dir-all-empties-a-directory-and-leaves-it.md`),
/// and a package whose directory survives its removal is a name that can never
/// be installed again.
fn remove_tree(dir: &Path) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("pkg: cannot read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("pkg: cannot read {}: {e}", dir.display()))?;
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            remove_tree(&path)?;
        } else {
            fs::remove_file(&path)
                .map_err(|e| format!("pkg: cannot remove {}: {e}", path.display()))?;
        }
    }
    fs::remove_dir(dir).map_err(|e| format!("pkg: cannot remove {}: {e}", dir.display()))
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
