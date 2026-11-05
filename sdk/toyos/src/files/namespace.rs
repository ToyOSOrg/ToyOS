//! A view's routing: which held capability an absolute path lies on, and the
//! read-only directory ancestors of the capabilities actually held. A path
//! beneath no capability and no such ancestor is `NotHeld`, so a process
//! given an empty view names nothing but an empty `/`.
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

use super::{Error, MAX_CAPABILITY_DEPTH};
use crate::fs;

/// Where an absolute, lexically clean path lies in a view.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Routed<C> {
    /// On the capability of the longest held prefix, at this path beneath it.
    Served(C, String),
    /// A directory [`Directories`] makes of the held capabilities' ancestors.
    Directory,
}

/// Route `abs` through `held`, which answers the capability a prefix names:
/// the longest prefix first, at most [`MAX_CAPABILITY_DEPTH`] components,
/// then `/`.
pub(super) fn route<'a, C>(
    abs: &str,
    dirs: impl FnOnce() -> Result<&'a Directories, Error>,
    mut held: impl FnMut(&str) -> Result<Option<C>, Error>,
) -> Result<Routed<C>, Error> {
    let parts: Vec<&str> = abs.split('/').filter(|p| !p.is_empty()).collect();
    for depth in (1..=parts.len().min(MAX_CAPABILITY_DEPTH)).rev() {
        let prefix = alloc::format!("/{}", parts[..depth].join("/"));
        if let Some(cap) = held(&prefix)? {
            return Ok(Routed::Served(cap, parts[depth..].join("/")));
        }
    }
    match held("/")? {
        Some(cap) => Ok(Routed::Served(cap, parts.join("/"))),
        None if dirs()?.node(abs).is_some() => Ok(Routed::Directory),
        None => Err(Error::Served(fs::Error::NotHeld)),
    }
}

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
            if parts.len() > MAX_CAPABILITY_DEPTH { continue; }
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

    /// `abs` routed in a view holding exactly `names`, each capability
    /// answered by its own prefix.
    fn routed(names: &[&str], abs: &str) -> Result<Routed<String>, Error> {
        let dirs = Directories::new(names.iter().map(|n| String::from(*n)));
        route(abs, || Ok(&dirs), |prefix| {
            let name = alloc::format!("{}{prefix}", fs::CAPABILITY_PREFIX);
            Ok(names.contains(&name.as_str()).then(|| String::from(prefix)))
        })
    }

    /// **A process given an empty view names nothing**: every directory any
    /// role serves, and the files a program would reach for first, are not
    /// held, and `/` is a directory listing nothing (above).
    #[test]
    fn an_empty_view_names_nothing_but_an_empty_root() {
        let not_held = Err(Error::Served(fs::Error::NotHeld));
        for abs in [
            "/system", "/system/bin/shell", "/system/etc/system.manifest", "/tmp", "/tmp/x", "/apps", "/apps/doom",
            "/home", "/home/toy", "/home/toy/Apps/doom", "/config", "/state", "/state/sshserver/authorized_keys",
            "/log", "/boot", "/dev/null",
        ] {
            assert_eq!(routed(&[], abs), not_held, "{abs}");
        }
        assert_eq!(routed(&[], "/"), Ok(Routed::Directory));
    }

    /// A view holding `/tmp` and a package's folder names what is beneath
    /// them on their own capabilities, their ancestors as directories, and
    /// nothing beside them.
    #[test]
    fn a_view_names_only_what_lies_beneath_what_it_holds() {
        let view = ["fs:/tmp", "fs:/home/toy/Apps/doom"];
        assert_eq!(routed(&view, "/tmp/a/b"), Ok(Routed::Served("/tmp".into(), "a/b".into())));
        assert_eq!(routed(&view, "/home/toy/Apps/doom/Data/x"), Ok(Routed::Served("/home/toy/Apps/doom".into(), "Data/x".into())));
        assert_eq!(routed(&view, "/home/toy/Apps"), Ok(Routed::Directory));
        for abs in ["/system/bin/shell", "/home/toy/Apps/other", "/home/toy/Documents", "/tmpx", "/state"] {
            assert_eq!(routed(&view, abs), Err(Error::Served(fs::Error::NotHeld)), "{abs}");
        }
    }
}
