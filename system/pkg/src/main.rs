//! `/system/bin/pkg` — the installer, and an ordinary program.
//!
//! **It holds no authority a shell does not.** It claims no device, receives no
//! service and is endowed nothing; it writes under `/apps` because `/apps` is
//! writable to it. What it does is verify an archive against the release's own
//! `SHA256SUMS`, ask, unpack, and write the `manifest.toml` that records which
//! binary the directory launches.
//!
//! **An archive is read twice and never held.** The first read hashes it and
//! plans it within the room `/apps`'s volume has; the second writes it, and has
//! to meet the plan header for header and the digest again at its end, so an
//! archive changed between the two leaves nothing installed.
//!
//! The decisions are `lib.rs`'s and are made on the host; what is left here is
//! the filesystem and the question put to the user.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

use flate2::bufread::MultiGzDecoder;
use pkg::archive::{self, Kind};
use pkg::{sums, Plan, Planner};
use toyos_manifest::package::{self, Package};

/// Answers the consent prompt in advance, for a caller with no terminal.
/// Spelled out rather than `-y`: installing without asking says so in full.
const ASSUME_YES: &str = "--yes";

/// The release publishes one covering every asset, so it is found by sitting
/// beside the archive rather than by being named on the command line.
const SUMS: &str = "SHA256SUMS";

/// How much of an archive one read takes, and of a file one write.
const CHUNK: usize = 1 << 20;

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

    let mut apps = apps()?;
    let room = apps.room().map_err(|e| format!("pkg: {} answers no room: {e:?}", package::DIR))?;
    let (planner, got) = survey(file, Planner::new(&name, room.free, room.unit))?;
    if got != want {
        return Err(format!("pkg: {name} hashes to {got} and {SUMS} says {want}"));
    }
    println!("pkg: verified {name} against {SUMS} ({got})");
    let plan = planner.finish(&got)?;
    let package = &plan.manifest;

    let dir = Package::dir(&package.name);
    if fs::symlink_metadata(&dir).is_ok() {
        return Err(format!(
            "pkg: {dir} is already there — remove it first, this installer does not replace"
        ));
    }
    if !assume_yes && !consented(package, file)? {
        return Err(format!("pkg: not installing {}", package.name));
    }
    let mut durable = || apps.sync().map_err(|e| format!("pkg: {}'s volume did not write what it was given: {e:?}", package::DIR));
    write_package(file, &plan, Path::new(package::DIR), &mut durable)?;
    println!(
        "pkg: installed {} {} at {dir} ({} bytes), launching {}",
        package.name, package.version, plan.need, package.program
    );
    Ok(())
}

/// `/apps`'s own capability, for what std has no word for: the room on its
/// volume, and a sync of the whole volume that answers for every file in it.
fn apps() -> Result<toyos::fs::Dir, String> {
    let names = toyos::endow::namespace()
        .ok_or_else(|| String::from("pkg: this process holds no directory capabilities"))?;
    let name = format!("{}{}", toyos::fs::CAPABILITY_PREFIX, package::DIR);
    toyos::fs::Dir::connect(names, &name).map_err(|e| format!("pkg: cannot reach {}: {e:?}", package::DIR))
}

/// The first read: every header through `planner`, and the whole file's
/// digest.
fn survey<'a>(file: &Path, mut planner: Planner<'a>) -> Result<(Planner<'a>, String), String> {
    let mut hashed = Hashed::open(file)?;
    let mut tar = archive::Reader::new(gunzip(&mut hashed));
    while let Some(header) = tar.next_entry()? {
        planner.add(header)?;
    }
    drop(tar);
    Ok((planner, hashed.finish()?))
}

/// Unpack `plan` from `file` under `root` and write its manifest last, or take
/// the package's directory back down: a verified archive can still run out of
/// volume half way, and a directory with no `manifest.toml` is a name `install`
/// refuses for the volume's life. Its directory is made first and alone, so a
/// name some other writer took is refused and never swept.
///
/// **`durable` is asked before the manifest is written**, and refuses if the
/// volume holds any file it could not write: a file's last close can be
/// refused, and std's `File` drops that answer.
fn write_package(
    file: &Path,
    plan: &Plan,
    root: &Path,
    durable: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let dir = root.join(&plan.manifest.name);
    fs::create_dir(&dir).map_err(|e| format!("pkg: cannot create {}: {e}", dir.display()))?;
    match unpack(file, plan, root).and_then(|()| durable()).and_then(|()| write_manifest(&dir, &plan.manifest)) {
        Ok(()) => Ok(()),
        Err(why) => match remove_tree(&dir) {
            Ok(()) => Err(format!("{why}; {} was taken back down", dir.display())),
            Err(swept) => Err(format!("{why}; {swept}")),
        },
    }
}

/// The second read, writing what the first one planned.
fn unpack(file: &Path, plan: &Plan, root: &Path) -> Result<(), String> {
    let changed = || format!("pkg: {} changed since it was verified", file.display());
    let mut hashed = Hashed::open(file)?;
    let mut tar = archive::Reader::new(gunzip(&mut hashed));
    let mut buf = vec![0u8; CHUNK];
    for want in &plan.entries {
        if tar.next_entry()?.as_ref() != Some(want) {
            return Err(changed());
        }
        let dest = root.join(&want.path);
        let parent = || match dest.parent() {
            Some(up) => fs::create_dir_all(up),
            None => Ok(()),
        };
        let made = match &want.kind {
            Kind::Dir => fs::create_dir_all(&dest).map_err(|e| e.to_string()),
            // The one link call std has for every OS this compiles for: ToyOS,
            // and the host its tests run on.
            #[allow(deprecated)]
            Kind::Symlink(target) => parent().and_then(|()| fs::soft_link(target, &dest)).map_err(|e| e.to_string()),
            Kind::File => parent()
                .and_then(|()| fs::File::create_new(&dest))
                .map_err(|e| e.to_string())
                .and_then(|out| copy_data(&mut tar, out, &mut buf)),
        };
        made.map_err(|e| format!("pkg: cannot write {}: {e}", dest.display()))?;
    }
    if tar.next_entry()?.is_some() {
        return Err(changed());
    }
    drop(tar);
    match hashed.finish()? == plan.manifest.digest {
        true => Ok(()),
        false => Err(changed()),
    }
}

/// The current entry's data, into `out`.
fn copy_data<R: Read>(tar: &mut archive::Reader<R>, mut out: fs::File, buf: &mut [u8]) -> Result<(), String> {
    loop {
        match tar.read_data(buf)? {
            0 => return Ok(()),
            n => out.write_all(&buf[..n]).map_err(|e| e.to_string())?,
        }
    }
}

/// The manifest, made durable before the install is called done: written last,
/// so a directory carrying one is a finished install, which the supervisor and
/// the launcher both reach a package through.
fn write_manifest(dir: &Path, package: &Package) -> Result<(), String> {
    let path = dir.join(package::FILE);
    let text = package.render()?;
    fs::File::create_new(&path)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.sync_all()))
        .map_err(|e| format!("pkg: cannot write {}: {e}", path.display()))
}

fn gunzip<R: Read>(r: R) -> MultiGzDecoder<BufReader<R>> {
    MultiGzDecoder::new(BufReader::with_capacity(CHUNK, r))
}

/// A file read through a SHA-256 of every byte read.
struct Hashed {
    file: fs::File,
    hash: toyos_crypto::hash::Sha256,
}

impl Hashed {
    fn open(path: &Path) -> Result<Self, String> {
        let file = fs::File::open(path).map_err(|e| format!("pkg: cannot read {}: {e}", path.display()))?;
        Ok(Self { file, hash: toyos_crypto::hash::Sha256::new() })
    }

    /// The whole file's digest: what was read, and the rest, read now. A tar
    /// ends before its file does, in the zeros gzip and tar pad it with, and
    /// `SHA256SUMS` covers every byte.
    fn finish(mut self) -> Result<String, String> {
        std::io::copy(&mut self, &mut std::io::sink()).map_err(|e| format!("pkg: {e}"))?;
        Ok(sums::hex(&self.hash.finalize()))
    }
}

impl Read for Hashed {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.file.read(buf)?;
        self.hash.update(&buf[..n]).map_err(|_| std::io::Error::other("a file past SHA-256's 2^61-byte bound"))?;
        Ok(n)
    }
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

/// Delete a directory and everything under it, a link as itself and never
/// what it points at.
///
/// **Not `fs::remove_dir_all`**: that one empties a directory and leaves it
/// (`issues/remove-dir-all-empties-a-directory-and-leaves-it.md`),
/// and a package whose directory survives its removal is a name that can never
/// be installed again.
///
/// **An explicit stack and no depth bound.** A bound only moved the harm: a
/// refusal past it leaves the directory standing, which is the unfreeable name
/// `install`'s unwind exists to prevent. Memory is bounded by the number of
/// directories instead, which no single path can spell.
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
    use toyos_tmpdir::TempDir;

    const ASSET: &str = "tool-v1.0-toyos-x86_64.tar.gz";

    /// A gzipped tar of `(path, data)` files and `(path, target)` links, as a
    /// writer outside this crate makes one.
    fn gzipped(files: &[(&str, &[u8])], links: &[(&str, &str)]) -> Vec<u8> {
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (path, data) in files {
            let mut header = tar::Header::new_ustar();
            header.set_size(data.len() as u64);
            header.set_mode(if path.ends_with("tool") { 0o755 } else { 0o644 });
            tar.append_data(&mut header, path, *data).expect("a file");
        }
        for (path, target) in links {
            let mut header = tar::Header::new_ustar();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            tar.append_link(&mut header, path, target).expect("a link");
        }
        tar.into_inner().and_then(|gz| gz.finish()).expect("an archive")
    }

    /// A toolchain's shape: its program in `bin`, a link to it, and a file
    /// past several reads, whose bytes `seed` decides.
    fn tool(seed: u32) -> Vec<u8> {
        let big: Vec<u8> = (0..3 * CHUNK as u32 + 7).map(|i| ((i + seed) * 31 % 251) as u8).collect();
        gzipped(&[("tool/bin/tool", b"ELF"), ("tool/share/big", &big)], &[("tool/bin/cc", "tool")])
    }

    /// The archive at `scratch/ASSET`, surveyed on a volume of `free` bytes.
    fn surveyed(scratch: &Path, bytes: &[u8], free: u64) -> Result<Plan, String> {
        let file = scratch.join(ASSET);
        fs::write(&file, bytes).unwrap();
        let (planner, digest) = survey(&file, Planner::new(ASSET, free, 4096))?;
        planner.finish(&digest)
    }

    #[test]
    fn an_archive_installs_whole_its_files_its_links_and_its_manifest() {
        let scratch = TempDir::new("pkg-install");
        let plan = surveyed(&scratch, &tool(0), 1 << 30).expect("a plan");
        assert_eq!(plan.manifest.program, "/apps/tool/bin/tool");
        let root = scratch.join("apps");
        fs::create_dir(&root).unwrap();
        write_package(&scratch.join(ASSET), &plan, &root, &mut || Ok(())).expect("installed");

        let big = fs::read(root.join("tool/share/big")).unwrap();
        assert_eq!(big.len(), 3 * CHUNK + 7);
        assert!(big.iter().enumerate().all(|(i, &b)| b == (i as u32 * 31 % 251) as u8));
        assert_eq!(fs::read_link(root.join("tool/bin/cc")).unwrap(), Path::new("tool"));
        assert_eq!(fs::read(root.join("tool/bin/cc")).unwrap(), b"ELF");
        let manifest = fs::read_to_string(root.join("tool/manifest.toml")).unwrap();
        assert_eq!(Package::parse(&manifest).unwrap(), plan.manifest);
    }

    /// An archive replaced between the read that verified it and the one that
    /// writes it installs nothing.
    #[test]
    fn an_archive_changed_after_its_survey_installs_nothing() {
        let scratch = TempDir::new("pkg-changed");
        let plan = surveyed(&scratch, &tool(0), 1 << 30).expect("a plan");
        let root = scratch.join("apps");
        fs::create_dir(&root).unwrap();
        let other = gzipped(&[("tool/bin/tool", b"ELF"), ("tool/share/big", b"short")], &[]);
        // The same headers over other bytes: only the digest tells.
        for replaced in [other, tool(1)] {
            fs::write(scratch.join(ASSET), &replaced).unwrap();
            let why = write_package(&scratch.join(ASSET), &plan, &root, &mut || Ok(())).expect_err("a changed archive installed");
            assert!(why.contains("was taken back down"), "{why}");
            assert!(!root.join("tool").exists(), "a changed archive left its directory: {why}");
        }
    }

    /// The second read writes only what the first one checked: an archive
    /// swapped for one that plants a link and writes through it reaches
    /// nothing outside the package, though its digest is only known at its end.
    #[test]
    fn a_swapped_archive_writes_nothing_its_plan_did_not_check() {
        let scratch = TempDir::new("pkg-swapped");
        let plan = surveyed(&scratch, &tool(0), 1 << 30).expect("a plan");
        let (root, outside) = (scratch.join("apps"), scratch.join("outside"));
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        let mut link = tar::Header::new_ustar();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        link.set_mode(0o777);
        tar.append_link(&mut link, "tool/out", "../../outside").unwrap();
        let mut file = tar::Header::new_ustar();
        file.set_size(1);
        file.set_mode(0o644);
        tar.append_data(&mut file, "tool/out/planted", &b"x"[..]).unwrap();
        fs::write(scratch.join(ASSET), tar.into_inner().and_then(|gz| gz.finish()).unwrap()).unwrap();

        let why = write_package(&scratch.join(ASSET), &plan, &root, &mut || Ok(())).expect_err("a swapped archive installed");
        assert!(why.contains("changed since it was verified"), "{why}");
        assert!(!outside.join("planted").exists(), "the swapped archive wrote through its link");
        assert!(!root.join("tool").exists());
    }

    /// The digest is of every byte of the file, past the tar's end and the
    /// gzip member's, which the archive's reader never asks for; a cut archive
    /// is refused where it ends.
    #[test]
    fn the_digest_covers_the_whole_file_and_a_cut_archive_is_refused() {
        let scratch = TempDir::new("pkg-truncated");
        let file = scratch.join(ASSET);
        let survey_of = |bytes: &[u8]| {
            fs::write(&file, bytes).unwrap();
            survey(&file, Planner::new(ASSET, 1 << 30, 4096)).map(|(_, digest)| digest)
        };
        let mut whole = tool(0);
        whole.extend(vec![0u8; 3 * CHUNK]);
        assert_eq!(survey_of(&whole).unwrap(), sums::hex(&toyos_crypto::hash::Sha256::digest(&whole).unwrap()));
        assert!(survey_of(&whole[..tool(0).len() / 3]).unwrap_err().contains("tar:"));
        assert!(survey_of(b"not gzip at all").is_err());
    }

    /// The room is the bound on what a survey inflates: a header claiming
    /// more is refused at that header, and nothing past it is read.
    #[test]
    fn an_archive_bigger_than_the_room_is_refused_unread() {
        let scratch = TempDir::new("pkg-room");
        let why = surveyed(&scratch, &tool(0), 64 * 1024).expect_err("3 MiB planned into 64 KiB");
        assert!(why.contains("needs more than the 65536 bytes free"), "{why}");

        // A bomb: a 4 GiB file of zeros, a few megabytes compressed, refused
        // at its header.
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        let mut header = tar::Header::new_ustar();
        header.set_path("tool/bin/tool").unwrap();
        header.set_size(4 << 30);
        header.set_mode(0o755);
        header.set_cksum();
        gz.write_all(header.as_bytes()).unwrap();
        let zeros = vec![0u8; 1 << 20];
        for _ in 0..64 {
            gz.write_all(&zeros).unwrap();
        }
        let bomb = gz.finish().unwrap();
        let why = surveyed(&scratch, &bomb, 1 << 30).expect_err("a 4 GiB entry planned into 1 GiB");
        assert!(why.contains("needs more than"), "{why}");
    }

    /// A volume that could not write every file of the package installs
    /// nothing, though every write was answered.
    #[test]
    fn a_volume_that_refuses_its_sync_installs_nothing() {
        let scratch = TempDir::new("pkg-durable");
        let plan = surveyed(&scratch, &tool(0), 1 << 30).expect("a plan");
        let root = scratch.join("apps");
        fs::create_dir(&root).unwrap();
        let mut refused = || Err(String::from("pkg: the volume did not write it"));
        let why = write_package(&scratch.join(ASSET), &plan, &root, &mut refused).expect_err("installed unwritten");
        assert!(why.contains("did not write it") && why.contains("was taken back down"), "{why}");
        assert!(!root.join("tool").exists());
    }

    /// A name some other writer holds is refused and left as it was found:
    /// only a directory this install made is one its unwind may take down.
    #[test]
    fn a_directory_already_at_the_name_is_refused_and_never_swept() {
        let scratch = TempDir::new("pkg-taken");
        let plan = surveyed(&scratch, &tool(0), 1 << 30).expect("a plan");
        let root = scratch.join("apps");
        fs::create_dir_all(root.join("tool")).unwrap();
        fs::write(root.join("tool/theirs"), b"x").unwrap();
        let why = write_package(&scratch.join(ASSET), &plan, &root, &mut || Ok(())).expect_err("an existing directory");
        assert!(why.contains("cannot create"), "{why}");
        assert!(root.join("tool/theirs").exists(), "a directory the installer did not make was swept");
    }

    /// 33 is the first depth the old bound refused; 256 is far past it.
    #[test]
    fn a_tree_of_any_depth_comes_off_and_leaves_no_directory() {
        for depth in [33usize, 256] {
            // The tree is inside the scratch, because its removal is what is judged.
            let scratch = TempDir::new(&format!("pkg-depth{depth}"));
            let root = scratch.join("tree");
            let mut at = root.clone();
            for _ in 0..depth {
                at = at.join("d");
            }
            fs::create_dir_all(&at).expect("a deep tree");
            fs::write(at.join("leaf"), b"x").expect("a leaf");
            // A second branch, so the walk is a tree and not a chain.
            fs::create_dir_all(root.join("wide/er")).expect("a second branch");
            fs::write(root.join("wide/er/leaf"), b"y").expect("a second leaf");
            // A link to outside the tree goes as itself.
            let outside = scratch.join("outside");
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("kept"), b"z").unwrap();
            #[allow(deprecated)]
            fs::soft_link(&outside, root.join("wide/out")).unwrap();

            remove_tree(&root).unwrap_or_else(|e| panic!("depth {depth}: {e}"));
            assert!(!root.exists(), "a tree {depth} deep survived its removal");
            assert!(outside.join("kept").exists(), "the removal followed a link");
        }
    }
}
