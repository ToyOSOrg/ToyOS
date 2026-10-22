//! What an archive installs, decided without touching a filesystem.
//!
//! **A package is a directory**, so installing one is: agree with the archive
//! on a single top-level name, find the program inside it that name promises,
//! check that it fits the room the volume has, and write the manifest
//! recording where the bytes came from. The archive is planned as it streams
//! past ([`Planner`]), and only a plan that finished is written.

pub mod archive;
pub mod sums;

use std::collections::BTreeMap;

use archive::{Header, Kind};
use toyos_manifest::package::{self, Package};

/// The suffix an installable archive carries. Not a guess about compression —
/// the version is read out of the name, so the name has a shape or it has none.
pub const SUFFIX: &str = ".tar.gz";

/// The most entries one archive installs. Policy on the primitive: the plan
/// holds every entry's header until the archive has been read to its end, so
/// this bounds the installer's memory. A toolchain is a few thousand.
pub const MAX_ENTRIES: usize = 1 << 16;

/// Everything the writer needs, and nothing it has to decide.
#[derive(Debug)]
pub struct Plan {
    pub manifest: Package,
    /// Every entry, in the archive's order, which the second read of the same
    /// archive must meet again header for header. **No mode is written**: this
    /// filesystem has no permission bits, so the archive's mode is read as its
    /// statement about which file is the program and is never written.
    pub entries: Vec<Header>,
    /// The room the files take on the volume, each rounded up to its unit.
    pub need: u64,
}

/// A plan, made one header at a time.
pub struct Planner<'a> {
    file_name: &'a str,
    free: u64,
    unit: u64,
    entries: Vec<Header>,
    /// Every path the archive names or implies, and whether it is a directory.
    /// One path is one thing: a path named twice, or a path beneath a file or a
    /// link, would decide by write order what is at it, or write through a
    /// link to somewhere the plan never checked.
    seen: BTreeMap<String, bool>,
    need: u64,
}

impl<'a> Planner<'a> {
    /// A plan for `file_name` on a volume with `free` bytes in units of `unit`.
    pub fn new(file_name: &'a str, free: u64, unit: u64) -> Self {
        Self { file_name, free, unit: unit.max(1), entries: Vec::new(), seen: BTreeMap::new(), need: 0 }
    }

    /// The next header, refused the moment it breaks the package's shape or
    /// takes the plan past the room: what an archive claims about its size is
    /// refused before any of that size is read.
    pub fn add(&mut self, header: Header) -> Result<(), String> {
        let name = self.file_name;
        if self.entries.len() == MAX_ENTRIES {
            return Err(format!("pkg: {name} holds more than {MAX_ENTRIES} entries"));
        }
        if let Some(first) = self.entries.first() {
            if header.top() != first.top() {
                return Err(format!(
                    "pkg: the archive holds both {:?} and {:?} at its top level",
                    first.top(),
                    header.top()
                ));
            }
        }
        if header.path == format!("{}/{}", header.top(), package::FILE) {
            return Err(format!("pkg: {name} carries {}, which is the installer's to write", header.path));
        }
        if let Kind::Symlink(target) = &header.kind {
            link_stays_inside(&header.path, target)
                .map_err(|why| format!("pkg: {name}'s link {} -> {target:?} {why}", header.path))?;
        }
        let parts: Vec<&str> = header.path.split('/').collect();
        for depth in 1..parts.len() {
            let parent = parts[..depth].join("/");
            if !*self.seen.entry(parent.clone()).or_insert(true) {
                return Err(format!("pkg: {name} puts {} beneath {parent}, which is not a directory", header.path));
            }
        }
        let is_dir = header.kind == Kind::Dir;
        match self.seen.insert(header.path.clone(), is_dir) {
            Some(true) if is_dir => {}
            Some(_) => return Err(format!("pkg: {name} names {} twice", header.path)),
            None => {}
        }
        self.need = self.need.saturating_add(header.size.div_ceil(self.unit).saturating_mul(self.unit));
        if self.need > self.free {
            return Err(format!(
                "pkg: {name} needs more than the {} bytes free under {}",
                self.free,
                package::DIR
            ));
        }
        self.entries.push(header);
        Ok(())
    }

    /// The plan, once the archive has ended, with the archive's `digest`.
    pub fn finish(self, digest: &str) -> Result<Plan, String> {
        let file_name = self.file_name;
        let name =
            self.entries.first().ok_or_else(|| String::from("pkg: the archive is empty"))?.top().to_string();
        let version = version_of(file_name, &name)?;
        let inside = program_of(&self.entries, &name)
            .map_err(|why| format!("pkg: {file_name} {why}"))?;
        let manifest = Package {
            name,
            version,
            digest: digest.to_string(),
            program: format!("{}/{inside}", package::DIR),
        };
        // The render is the check: it is what refuses a name, a version or a
        // program that would not come back out of the file the same way.
        manifest.render()?;
        Ok(Plan { manifest, entries: self.entries, need: self.need })
    }
}

/// The program a package launches: `<name>/<name>`, as a package of one
/// program has it, or `<name>/bin/<name>`, as a toolchain does, an executable
/// file either way. Both is a package that has not said which.
fn program_of(entries: &[Header], name: &str) -> Result<String, String> {
    let candidates = [format!("{name}/{name}"), format!("{name}/bin/{name}")];
    let found: Vec<&Header> =
        entries.iter().filter(|e| candidates.contains(&e.path)).collect();
    match found[..] {
        [one] if one.kind == Kind::File && one.executable() => Ok(one.path.clone()),
        [one] => Err(format!("has {} and it is not an executable file", one.path)),
        [] => Err(format!("carries neither {} nor {}, so there is no program to launch", candidates[0], candidates[1])),
        _ => Err(format!("carries both {} and {}, so which one launches is not said", candidates[0], candidates[1])),
    }
}

/// Whether the symlink at `path` reaching `target` lands inside the package.
///
/// **Only `..` first and names after**: each leading `..` climbs one real
/// directory of the link's own path, which [`Planner::add`] has made sure is a
/// directory and not a link, and the climb may not pass the package's own
/// directory. A `..` after a name would climb from wherever that name, perhaps
/// itself a link, resolves to, which no reading of the archive alone can say.
fn link_stays_inside(path: &str, target: &str) -> Result<(), &'static str> {
    if target.starts_with('/') {
        return Err("is absolute");
    }
    let parts: Vec<&str> = target.split('/').collect();
    let climbs = parts.iter().take_while(|p| **p == "..").count();
    if parts[climbs..].iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
        return Err("has an empty, `.` or `..` component after a name");
    }
    // `top/a/link` is in two directories of the package, `top/a` and `top`;
    // `top` itself is in none.
    let depth = path.split('/').count().checked_sub(2).ok_or("is the package's own directory")?;
    if climbs > depth {
        return Err("climbs out of the package");
    }
    Ok(())
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
    const ROOMY: u64 = 1 << 40;

    fn entry(path: &str, kind: Kind, size: u64, mode: u32) -> Header {
        Header { path: path.to_string(), kind, size, mode }
    }

    fn link(path: &str, target: &str) -> Header {
        entry(path, Kind::Symlink(target.to_string()), 0, 0o777)
    }

    fn gbae() -> Vec<Header> {
        vec![
            entry("gbae", Kind::Dir, 0, 0o755),
            entry("gbae/gbae", Kind::File, 6, 0o755),
            entry("gbae/README.md", Kind::File, 6, 0o644),
            entry("gbae/LICENSE", Kind::File, 3, 0o644),
        ]
    }

    fn plan_of(file_name: &str, entries: Vec<Header>, free: u64) -> Result<Plan, String> {
        let mut planner = Planner::new(file_name, free, 4096);
        for e in entries {
            planner.add(e)?;
        }
        planner.finish(DIGEST)
    }

    #[test]
    fn the_real_asset_plans_the_directory_the_track_describes() {
        let plan = plan_of(ASSET, gbae(), ROOMY).expect("plan");
        assert_eq!(plan.manifest.name, "gbae");
        assert_eq!(plan.manifest.version, "0.2.0");
        assert_eq!(plan.manifest.digest, DIGEST);
        assert_eq!(plan.manifest.program, "/apps/gbae/gbae");
        let paths: Vec<&str> = plan.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["gbae", "gbae/gbae", "gbae/README.md", "gbae/LICENSE"]);
        assert_eq!(plan.need, 3 * 4096);
    }

    /// A toolchain's shape: its program in `bin`, beside a link to it, and no
    /// explicit directory entries at all.
    #[test]
    fn a_toolchain_launches_its_bin_and_carries_its_links() {
        let entries = vec![
            entry("clang/bin/clang", Kind::File, 10_000, 0o755),
            link("clang/bin/cc", "clang"),
            link("clang/lib/clang/22/x", "../../../bin/clang"),
            entry("clang/sysroot/lib/libtoyos_c.a", Kind::File, 1, 0o644),
        ];
        let plan = plan_of("clang-v22.1.0-toyos-x86_64.tar.gz", entries, ROOMY).expect("plan");
        assert_eq!(plan.manifest.program, "/apps/clang/bin/clang");
        assert_eq!(plan.manifest.version, "22.1.0");
        assert_eq!(plan.need, 3 * 4096 + 4096);
    }

    #[test]
    fn an_archive_that_is_not_one_directory_or_has_no_one_program_installs_nothing() {
        let mut two = gbae();
        two.push(entry("other/x", Kind::File, 1, 0o644));
        assert!(plan_of(ASSET, two, ROOMY).unwrap_err().contains("at its top level"));
        assert!(plan_of(ASSET, Vec::new(), ROOMY).is_err());

        let no_program = vec![entry("gbae", Kind::Dir, 0, 0o755), entry("gbae/README.md", Kind::File, 6, 0o644)];
        assert!(plan_of(ASSET, no_program, ROOMY).unwrap_err().contains("no program to launch"));

        // A directory where the program should be is not a program, and
        // neither is a file the archive never marked executable, nor a link.
        for program in [
            entry("gbae/gbae", Kind::Dir, 0, 0o755),
            entry("gbae/gbae", Kind::File, 1, 0o644),
            link("gbae/gbae", "README.md"),
        ] {
            let why = plan_of(ASSET, vec![entry("gbae", Kind::Dir, 0, 0o755), program], ROOMY).unwrap_err();
            assert!(why.contains("not an executable file"), "{why}");
        }

        let mut both = gbae();
        both.push(entry("gbae/bin/gbae", Kind::File, 1, 0o755));
        assert!(plan_of(ASSET, both, ROOMY).unwrap_err().contains("which one launches"));
    }

    #[test]
    fn a_path_named_twice_or_beneath_a_file_or_a_link_is_refused() {
        for extra in [
            entry("gbae/README.md", Kind::Dir, 0, 0o755),
            entry("gbae/LICENSE", Kind::File, 5, 0o644),
            link("gbae/LICENSE", "README.md"),
            entry("gbae/gbae", Kind::Dir, 0, 0o755),
            entry("gbae", Kind::File, 1, 0o644),
        ] {
            let mut twice = gbae();
            twice.push(extra.clone());
            let why = plan_of(ASSET, twice, ROOMY).expect_err(&format!("{extra:?} was planned"));
            assert!(why.contains("twice"), "{why}");
        }
        let mut manifest = gbae();
        manifest.push(entry("gbae/manifest.toml", Kind::File, 1, 0o644));
        assert!(plan_of(ASSET, manifest, ROOMY).unwrap_err().contains("the installer's to write"));

        // A directory given again is the same directory.
        let mut again = gbae();
        again.push(entry("gbae", Kind::Dir, 0, 0o755));
        assert!(plan_of(ASSET, again, ROOMY).is_ok());

        // Written beneath a link, a file lands wherever the link points.
        for beneath in [
            vec![link("gbae/lib", "share"), entry("gbae/lib/x", Kind::File, 1, 0o644)],
            vec![link("gbae/a/up", ".."), entry("gbae/a/up/b/x", Kind::File, 1, 0o644)],
            vec![entry("gbae/LICENSE/x", Kind::File, 1, 0o644)],
        ] {
            let mut entries = gbae();
            entries.extend(beneath);
            let why = plan_of(ASSET, entries, ROOMY).unwrap_err();
            assert!(why.contains("which is not a directory"), "{why}");
        }
    }

    /// The links a package may carry are the ones that resolve inside it,
    /// whatever the links around them resolve to.
    #[test]
    fn a_link_whose_target_leaves_the_package_is_refused() {
        let top = plan_of(ASSET, vec![link("gbae", "elsewhere")], ROOMY).unwrap_err();
        assert!(top.contains("the package's own directory"), "{top}");
        for (path, target) in [
            ("gbae/x", "/system/bin/supervisor"),
            ("gbae/x", ".."),
            ("gbae/x", "../other/other"),
            ("gbae/a/x", "../../.."),
            // `a/..` is `gbae` read as text, and wherever `a`'s link points
            // read as the filesystem does.
            ("gbae/x", "a/../.."),
            ("gbae/x", "a/.."),
            ("gbae/x", "./gbae"),
            ("gbae/x", "a//b"),
        ] {
            let mut entries = gbae();
            entries.push(link(path, target));
            let why = plan_of(ASSET, entries, ROOMY).expect_err(&format!("{path} -> {target} was planned"));
            assert!(why.contains("link"), "{why}");
        }
        for (path, target) in [("gbae/x", "gbae"), ("gbae/a/x", ".."), ("gbae/a/b/x", "../../gbae"), ("gbae/a/x", "../b/c")]
        {
            let mut entries = gbae();
            entries.push(link(path, target));
            assert!(plan_of(ASSET, entries, ROOMY).is_ok(), "{path} -> {target} was refused");
        }
    }

    /// What does not fit is refused at the header that claims it, before a
    /// byte of its data is read.
    #[test]
    fn an_entry_past_the_room_or_past_the_count_is_refused_at_its_header() {
        let mut planner = Planner::new(ASSET, 2 * 4096, 4096);
        planner.add(entry("gbae/gbae", Kind::File, 4096, 0o755)).unwrap();
        planner.add(entry("gbae/a", Kind::File, 1, 0o644)).unwrap();
        let why = planner.add(entry("gbae/b", Kind::File, 1, 0o644)).unwrap_err();
        assert!(why.contains("needs more than the 8192 bytes free"), "{why}");

        let huge = entry("gbae/gbae", Kind::File, u64::MAX / 2, 0o755);
        assert!(Planner::new(ASSET, ROOMY, 4096).add(huge).is_err());

        let mut many = Planner::new(ASSET, ROOMY, 4096);
        many.add(entry("gbae", Kind::Dir, 0, 0o755)).unwrap();
        for i in 1..MAX_ENTRIES {
            many.add(entry(&format!("gbae/{i}"), Kind::Dir, 0, 0o755)).unwrap();
        }
        assert!(many.add(entry("gbae/last", Kind::Dir, 0, 0o755)).unwrap_err().contains("more than"));
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
    /// producing a directory the supervisor cannot resolve.
    #[test]
    fn a_digest_the_manifest_cannot_carry_stops_the_install() {
        let planned = |digest: &str| {
            let mut planner = Planner::new(ASSET, ROOMY, 4096);
            gbae().into_iter().try_for_each(|e| planner.add(e)).unwrap();
            planner.finish(digest)
        };
        assert!(planned(DIGEST).is_ok());
        assert!(planned("not-a-digest").is_err());
        assert!(planned(&DIGEST.to_uppercase()).is_err());
    }
}
