//! Every path a client names, resolved inside the directory its connection was
//! given, and nowhere else.
//!
//! **The root is a floor, not a starting point.** A connection is bound to one
//! directory of the volume (its capability's); a client's path is canonical
//! and relative to it (`toyos::fs::canonical`), and each component is looked up
//! in turn. A symlink met on the way is expanded where it stands: a relative
//! target is read against the link's own directory, and a `..` that would
//! climb above the connection's root is refused — [`Escape`] — rather than
//! clamped, because a clamped path is a different file from the one the link
//! named. An absolute target is not this server's to resolve: it names a path
//! in the *client's* table, which may hold directories this connection does
//! not, and the client resolves it again there ([`Resolved::Absolute`]).
//!
//! **One resolution sees one volume.** The server is one thread and a request
//! is answered before the next is read, so no rename can land between two
//! component lookups of the same path.
//!
//! [`Escape`]: Refusal::Escape

use crate::volume::join;

/// The most symlinks one resolution expands, as Linux's `MAXSYMLINKS`.
pub const MAX_LINKS: u32 = 40;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    /// The volume path the request acts on.
    Path(String),
    /// The path met an absolute symlink: resolve this, in the client's table.
    Absolute(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A link's `..` climbed above the connection's root.
    Escape,
    /// More than [`MAX_LINKS`] links, or a cycle.
    Loop,
    /// A name on the way is not a directory, or a link target is empty.
    NotFound,
    /// The volume would not answer a lookup.
    Io,
}

/// What a lookup of one volume path found: a symlink's target, or anything
/// else (a file, a directory, or nothing, which the operation judges).
pub enum Found {
    Link(String),
    Other,
}

/// Resolve `rel` under `root`. `follow_last` follows a symlink in the final
/// component too; an operation on the link itself (`lstat`, `unlink`,
/// `readlink`, a `rename`'s ends) passes `false`.
pub fn resolve(
    root: &str,
    rel: &str,
    follow_last: bool,
    lookup: &mut dyn FnMut(&str) -> Result<Found, ()>,
) -> Result<Resolved, Refusal> {
    // What is resolved so far, as components under `root`, and what is left.
    let mut done: Vec<String> = Vec::new();
    let mut left: Vec<String> = rel.split('/').filter(|c| !c.is_empty()).rev().map(String::from).collect();
    let mut links = 0;
    while let Some(component) = left.pop() {
        match component.as_str() {
            "." => continue,
            ".." => {
                // Only a link's target carries one: the wire refuses them.
                if done.pop().is_none() {
                    return Err(Refusal::Escape);
                }
                continue;
            }
            _ => {}
        }
        done.push(component);
        let last = left.is_empty();
        if last && !follow_last {
            break;
        }
        let path = join(root, &done.join("/"));
        let target = match lookup(&path).map_err(|()| Refusal::Io)? {
            Found::Other => continue,
            Found::Link(target) => target,
        };
        links += 1;
        if links > MAX_LINKS {
            return Err(Refusal::Loop);
        }
        if target.is_empty() {
            return Err(Refusal::NotFound);
        }
        done.pop();
        if let Some(absolute) = target.strip_prefix('/') {
            let mut rest: Vec<&str> = left.iter().rev().map(String::as_str).collect();
            rest.insert(0, absolute.trim_end_matches('/'));
            let joined = rest.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("/");
            return Ok(Resolved::Absolute(format!("/{joined}")));
        }
        for part in target.split('/').filter(|c| !c.is_empty()).rev() {
            left.push(part.to_string());
        }
    }
    Ok(Resolved::Path(join(root, &done.join("/"))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A volume of symlinks: every name not in the map is "something else".
    fn vol(links: &[(&str, &str)]) -> BTreeMap<String, String> {
        links.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    fn run(links: &BTreeMap<String, String>, root: &str, rel: &str, follow: bool) -> Result<Resolved, Refusal> {
        resolve(root, rel, follow, &mut |path| {
            Ok(links.get(path).map_or(Found::Other, |t| Found::Link(t.clone())))
        })
    }

    fn path(p: &str) -> Result<Resolved, Refusal> {
        Ok(Resolved::Path(p.to_string()))
    }

    #[test]
    fn a_plain_path_lands_under_the_root() {
        let v = vol(&[]);
        assert_eq!(run(&v, "home", "toy/notes.txt", true), path("home/toy/notes.txt"));
        assert_eq!(run(&v, "", "boot.log", true), path("boot.log"));
        assert_eq!(run(&v, "home", "", true), path("home"));
    }

    #[test]
    fn a_relative_link_is_read_against_its_own_directory() {
        let v = vol(&[("home/toy/docs", "Documents"), ("home/toy/up", "../toy/Documents")]);
        assert_eq!(run(&v, "home", "toy/docs/a", true), path("home/toy/Documents/a"));
        assert_eq!(run(&v, "home", "toy/up/b", true), path("home/toy/Documents/b"));
    }

    /// The escape suite's first half: every `..` the literature names that
    /// would leave the connection's directory, through a link, is refused.
    #[test]
    fn no_link_climbs_out_of_the_root() {
        let v = vol(&[
            ("home/toy/out", "../../state/sshserver"),
            ("home/toy/deep", "a/../../../etc"),
            ("home/up", ".."),
            ("home/dotdot", "../home/../../x"),
            ("home/toy/chain1", "chain2"),
            ("home/toy/chain2", "../../apps"),
        ]);
        for rel in ["toy/out", "toy/out/authorized_keys", "toy/deep", "up", "up/x", "dotdot", "toy/chain1/y"] {
            assert_eq!(run(&v, "home", rel, true), Err(Refusal::Escape), "{rel}");
        }
        // A link to its own root's parent from the root itself.
        assert_eq!(run(&vol(&[("x", "..")]), "", "x", true), Err(Refusal::Escape));
    }

    #[test]
    fn a_link_that_stays_inside_by_going_up_and_back_resolves() {
        let v = vol(&[("home/toy/a/back", "../b")]);
        assert_eq!(run(&v, "home", "toy/a/back/f", true), path("home/toy/b/f"));
    }

    #[test]
    fn an_absolute_link_is_handed_back_with_the_rest_of_the_path() {
        let v = vol(&[("home/toy/sys", "/system/share")]);
        assert_eq!(
            run(&v, "home", "toy/sys/fonts/a.ttf", true),
            Ok(Resolved::Absolute("/system/share/fonts/a.ttf".to_string()))
        );
        assert_eq!(run(&v, "home", "toy/sys", true), Ok(Resolved::Absolute("/system/share".to_string())));
    }

    #[test]
    fn the_last_link_is_left_alone_when_asked() {
        let v = vol(&[("home/l", "../../etc")]);
        assert_eq!(run(&v, "home", "l", false), path("home/l"));
    }

    #[test]
    fn a_cycle_is_refused_and_not_followed_for_ever() {
        let v = vol(&[("a", "b"), ("b", "a")]);
        assert_eq!(run(&v, "", "a", true), Err(Refusal::Loop));
    }

    #[test]
    fn an_empty_target_names_nothing() {
        assert_eq!(run(&vol(&[("a", "")]), "", "a", true), Err(Refusal::NotFound));
    }
}
