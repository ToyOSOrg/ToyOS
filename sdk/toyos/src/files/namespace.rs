//! Read-only directory ancestors of the filesystem capabilities actually held.
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

pub(super) struct Directories(Vec<String>);

impl Directories {
    pub(super) fn new(names: impl IntoIterator<Item = String>) -> Self {
        let mut paths = BTreeSet::from([String::from("/")]);
        for name in names {
            let Some(path) = name.strip_prefix(crate::fs::CAPABILITY_PREFIX) else { continue };
            if !path.starts_with('/') || super::absolute("/", path) != path {
                continue;
            }
            let parts: Vec<_> = path.split('/').filter(|p| !p.is_empty()).collect();
            if parts.len() > super::MAX_CAPABILITY_DEPTH { continue; }
            for end in 1..=parts.len() {
                paths.insert(alloc::format!("/{}", parts[..end].join("/")));
            }
        }
        Self(paths.into_iter().collect())
    }

    pub(super) fn node(&self, path: &str) -> Option<u64> {
        self.0.binary_search_by(|name| name.as_str().cmp(path)).ok().map(|i| i as u64 + 1)
    }

    pub(super) fn children<'a>(&'a self, path: &'a str) -> impl Iterator<Item = (&'a str, u64)> {
        self.0.iter().enumerate().filter_map(move |(i, child)| {
            let rest = child.strip_prefix(path)?;
            let rest = if path == "/" { rest } else { rest.strip_prefix('/')? };
            (!rest.is_empty() && !rest.contains('/')).then_some((rest, i as u64 + 1))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_held_filesystem_names_create_directories() {
        let dirs = Directories::new(["fs:/home/jan", "fs:/tmp", "fs:/home/jan/Games", "net", "fs:relative", "fs:/bad/../name", "fs:/a/b/c/d/e"].map(String::from));
        assert_eq!(dirs.children("/").map(|(s, _)| s).collect::<Vec<_>>(), ["home", "tmp"]);
        assert_eq!(dirs.children("/home").map(|(s, _)| s).collect::<Vec<_>>(), ["jan"]);
        assert!(dirs.node("/home/jane").is_none());
        assert!(dirs.node("/system").is_none());
        assert_eq!(dirs.node("/"), Some(1));
    }

    #[test]
    fn an_empty_namespace_has_an_empty_root() {
        let dirs = Directories::new([]);
        assert_eq!(dirs.node("/"), Some(1));
        assert_eq!(dirs.children("/").count(), 0);
    }
}
