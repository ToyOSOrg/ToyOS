//! What an archive installs, decided without touching a filesystem.
//!
//! **A package is a directory**, so installing one is: agree with the archive
//! on a single top-level name, find the program inside it that name promises,
//! and write the manifest recording where the bytes came from.

pub mod archive;
pub mod sums;

use archive::{Entry, Kind};
use toyos_manifest::package::{self, Package};

/// The suffix an installable archive carries. Not a guess about compression —
/// the version is read out of the name, so the name has a shape or it has none.
pub const SUFFIX: &str = ".tar.gz";

/// Everything the writer needs, and nothing it has to decide.
pub struct Plan<'a> {
    pub manifest: Package,
    /// Directories to create, shallowest first.
    pub dirs: Vec<String>,
    /// `(whole guest path, contents)`. **No mode travels with them**: this
    /// filesystem has no permission bits, so the archive's mode is read as its
    /// statement about which file is the program and is never written.
    pub files: Vec<(String, &'a [u8])>,
}

/// What installing `file_name` would do, or why it would not.
pub fn plan<'a>(
    file_name: &str,
    digest: &str,
    entries: &[Entry<'a>],
) -> Result<Plan<'a>, String> {
    let name = single_top(entries)?;
    let version = version_of(file_name, name)?;
    let program = format!("{}/{name}", Package::dir(name));
    let inside = format!("{name}/{name}");
    match entries.iter().find(|e| e.path == inside) {
        Some(e) if e.kind == Kind::File && e.executable() => {}
        Some(_) => {
            return Err(format!("pkg: {file_name}'s {inside} is not an executable file"));
        }
        None => {
            return Err(format!(
                "pkg: {file_name} carries no {inside}, so there is no program to launch"
            ))
        }
    }

    let mut dirs = vec![Package::dir(name)];
    let mut files = Vec::new();
    for entry in entries {
        let dest = format!("{}/{}", package::DIR, entry.path);
        match entry.kind {
            Kind::Dir => dirs.push(dest),
            Kind::File => files.push((dest, entry.data)),
        }
    }
    dirs.sort();
    dirs.dedup();
    // One path, one kind, over both lists: the write order is directories then
    // files, so a path named as both would decide by that order what is at it.
    let mut seen: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    seen.extend(dirs.iter().map(String::as_str));
    seen.sort_unstable();
    let count = seen.len();
    seen.dedup();
    if seen.len() != count {
        return Err(format!("pkg: {file_name} names one path twice"));
    }

    let manifest = Package {
        name: name.to_string(),
        version,
        digest: digest.to_string(),
        program,
    };
    // The render is the check: it is what refuses a name, a version or a
    // program that would not come back out of the file the same way.
    manifest.render()?;
    Ok(Plan { manifest, dirs, files })
}

/// The one directory every entry is inside, which is the package's name. An
/// archive that spreads over two top-level names installs neither: picking the
/// first would install half of it under a name it never chose.
fn single_top<'a>(entries: &'a [Entry<'a>]) -> Result<&'a str, String> {
    let first = entries.first().ok_or_else(|| String::from("pkg: the archive is empty"))?.top();
    match entries.iter().map(Entry::top).find(|top| *top != first) {
        Some(other) => {
            Err(format!("pkg: the archive holds both {first:?} and {other:?} at its top level"))
        }
        None => Ok(first),
    }
}

/// The version, read out of the release's own file name.
/// `<name>-v<version>-<platform>.tar.gz` is the shape this installer takes;
/// anything else is refused rather than installed with a version nobody stated.
pub fn version_of(file_name: &str, name: &str) -> Result<String, String> {
    let stem = file_name
        .strip_suffix(SUFFIX)
        .ok_or_else(|| format!("pkg: {file_name:?} does not end {SUFFIX}"))?;
    let rest = stem
        .strip_prefix(&format!("{name}-v"))
        .ok_or_else(|| format!("pkg: {file_name:?} does not begin `{name}-v<version>`"))?;
    let version = rest.split('-').next().unwrap_or("");
    if version.is_empty() {
        return Err(format!("pkg: {file_name:?} names no version after `{name}-v`"));
    }
    Ok(version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "99fcd8a7263b5c25cd90cead1baaa7200ef272100fc2226e008a4e8205ba2916";
    const ASSET: &str = "gbae-v0.2.0-toyos-x86_64.tar.gz";

    fn entry(path: &str, kind: Kind, data: &'static [u8], mode: u32) -> Entry<'static> {
        Entry { path: path.to_string(), kind, data, mode }
    }

    fn gbae() -> Vec<Entry<'static>> {
        vec![
            entry("gbae", Kind::Dir, b"", 0o755),
            entry("gbae/gbae", Kind::File, b"ELF...", 0o755),
            entry("gbae/README.md", Kind::File, b"# gbae", 0o644),
            entry("gbae/LICENSE", Kind::File, b"MIT", 0o644),
        ]
    }

    #[test]
    fn the_real_asset_plans_the_directory_the_track_describes() {
        let plan = plan(ASSET, DIGEST, &gbae()).expect("plan");
        assert_eq!(plan.manifest.name, "gbae");
        assert_eq!(plan.manifest.version, "0.2.0");
        assert_eq!(plan.manifest.digest, DIGEST);
        assert_eq!(plan.manifest.program, "/apps/gbae/gbae");
        assert_eq!(plan.dirs, ["/apps/gbae"]);
        let paths: Vec<&str> = plan.files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, ["/apps/gbae/gbae", "/apps/gbae/README.md", "/apps/gbae/LICENSE"]);
    }

    #[test]
    fn an_archive_that_is_not_one_directory_installs_nothing() {
        let mut two = gbae();
        two.push(entry("other/x", Kind::File, b"x", 0o644));
        assert!(plan(ASSET, DIGEST, &two).is_err());
        assert!(plan(ASSET, DIGEST, &[]).is_err());

        let no_program = vec![
            entry("gbae", Kind::Dir, b"", 0o755),
            entry("gbae/README.md", Kind::File, b"# gbae", 0o644),
        ];
        assert!(plan(ASSET, DIGEST, &no_program).is_err());

        // A directory where the program should be is not a program, and
        // neither is a file the archive never marked executable.
        let dir_instead =
            vec![entry("gbae", Kind::Dir, b"", 0o755), entry("gbae/gbae", Kind::Dir, b"", 0o755)];
        assert!(plan(ASSET, DIGEST, &dir_instead).is_err());
        let not_executable =
            vec![entry("gbae", Kind::Dir, b"", 0o755), entry("gbae/gbae", Kind::File, b"x", 0o644)];
        assert!(plan(ASSET, DIGEST, &not_executable).is_err());
    }

    #[test]
    fn a_path_the_archive_names_as_both_a_file_and_a_directory_is_refused() {
        let mut both = gbae();
        both.push(entry("gbae/README.md", Kind::Dir, b"", 0o755));
        let why = plan(ASSET, DIGEST, &both).err().expect("a path named twice was planned");
        assert!(why.contains("names one path twice"), "{why}");

        let mut twice = gbae();
        twice.push(entry("gbae/LICENSE", Kind::File, b"other", 0o644));
        assert!(plan(ASSET, DIGEST, &twice).is_err());

        let mut program = gbae();
        program.push(entry("gbae/gbae", Kind::Dir, b"", 0o755));
        assert!(plan(ASSET, DIGEST, &program).is_err());
    }

    #[test]
    fn a_file_name_that_states_no_version_is_refused() {
        assert_eq!(version_of(ASSET, "gbae").unwrap(), "0.2.0");
        assert_eq!(version_of("foo-v1.0.tar.gz", "foo").unwrap(), "1.0");
        assert!(version_of("gbae.tar.gz", "gbae").is_err());
        assert!(version_of("gbae-v-toyos.tar.gz", "gbae").is_err());
        assert!(version_of("gbae-v0.2.0-toyos.zip", "gbae").is_err());
        assert!(version_of(ASSET, "other").is_err());
    }

    /// A digest the manifest could not carry stops the install rather than
    /// producing a directory init cannot resolve.
    #[test]
    fn a_digest_the_manifest_cannot_carry_stops_the_install() {
        assert!(plan(ASSET, "not-a-digest", &gbae()).is_err());
        assert!(plan(ASSET, &DIGEST.to_uppercase(), &gbae()).is_err());
    }
}
