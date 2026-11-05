//! Every path a client names, resolved inside the directory its connection was
//! given, and nowhere else.
//!
//! **The root is a floor, not a starting point.** A connection is bound to one
//! directory of the volume (its grant's root); a client's path is relative
//! (`toyos::fs::relative`) to that directory or to one beneath it the client
//! holds open, and each component is looked up in turn. A `..` is the parent
//! of the directory resolved so far, and a symlink met on the way is expanded
//! where it stands: a relative target is read against the link's own
//! directory. A `..`, the client's or a link's, that would climb above the
//! connection's root is refused — [`Escape`] — rather than clamped, because a
//! clamped path is a different file from the one named. An absolute target
//! is not this server's to resolve: it names a path in the *client's*
//! table, which may hold directories this connection does not, and the client
//! resolves it again there ([`Resolved::Absolute`]).
//!
//! **One resolution sees one volume.** The server is one thread and a request
//! is answered before the next is read, so no rename can land between two
//! component lookups of the same path. A rename between two requests moves a
//! held directory where the volume moved it (`descriptions::renamed`), and a
//! start a rename left outside the root is refused [`Escape`] like a `..`.
//!
//! [`Escape`]: Refusal::Escape

use crate::volume::join;

/// The most symlinks one resolution expands: POSIX's `{SYMLOOP_MAX}`, which
/// may be no less than `{_POSIX_SYMLOOP_MAX}`, 8. No document names 40:
/// `issues/values-no-document-states-stand-where-another-os-was-their-source.md`.
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
    /// A `..` climbed above the connection's root, or a rename left the
    /// directory a path starts from outside it.
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

/// Resolve `rel` from `start`, the root or a held directory, which is refused
/// [`Refusal::Escape`] unless it is at or beneath `root`. `follow_last`
/// follows a symlink in the final component too; an operation on the link
/// itself (`lstat`, `unlink`, `readlink`, a `rename`'s ends) passes `false`.
pub fn resolve(
    root: &str,
    start: &str,
    rel: &str,
    follow_last: bool,
    lookup: &mut dyn FnMut(&str) -> Result<Found, ()>,
) -> Result<Resolved, Refusal> {
    let below = match (root, start.strip_prefix(root)) {
        ("", _) => start,
        (_, Some("")) => "",
        (_, Some(rest)) if rest.starts_with('/') => rest,
        _ => return Err(Refusal::Escape),
    };
    // What is resolved so far, as components under `root`, and what is left.
    let mut done: Vec<String> = below.split('/').filter(|c| !c.is_empty()).map(String::from).collect();
    let mut left: Vec<String> = rel.split('/').filter(|c| !c.is_empty()).rev().map(String::from).collect();
    let mut links = 0;
    while let Some(component) = left.pop() {
        match component.as_str() {
            "." => continue,
            ".." => {
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
        resolve(root, root, rel, follow, &mut |path| {
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

    /// The same suite under a folder of the home a package is granted: no
    /// link inside it reaches the home around it, another app's folder or
    /// the volume's other directories, and one to an absolute path is the
    /// package's own table's to resolve, where none of those is.
    #[test]
    fn no_link_climbs_out_of_a_granted_folder() {
        let root = "home/toy/Games";
        let v = vol(&[
            ("home/toy/Games/up", ".."),
            ("home/toy/Games/home", "../.."),
            ("home/toy/Games/other", "../Apps/other/Data"),
            ("home/toy/Games/GBA/state", "../../../../state/supervisor"),
            ("home/toy/Games/GBA/self", "../../Games/GBA"),
        ]);
        for rel in ["up", "up/Documents", "home", "other", "other/kept", "GBA/state/grants", "GBA/self"] {
            assert_eq!(run(&v, root, rel, true), Err(Refusal::Escape), "{rel}");
        }
        let abs = vol(&[("home/toy/Games/abs", "/home/toy/Apps/other")]);
        assert_eq!(run(&abs, root, "abs/kept", true), Ok(Resolved::Absolute("/home/toy/Apps/other/kept".to_string())));
        assert_eq!(run(&vol(&[("home/toy/Games/GBA/back", "../ROMs")]), root, "GBA/back/a.gba", true), path("home/toy/Games/ROMs/a.gba"));
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

    /// A client's `..` climbs from the directory it starts in, through the
    /// directory a link resolved to rather than the link's own, and never
    /// above the root.
    #[test]
    fn a_client_s_climb_is_by_directory_and_stops_at_the_root() {
        let v = vol(&[("home/toy/docs", "a/b")]);
        let from = |start: &str, rel: &str| {
            resolve("home", start, rel, true, &mut |path| Ok(v.get(path).map_or(Found::Other, |t| Found::Link(t.clone()))))
        };
        assert_eq!(from("home/toy", "../x"), path("home/x"));
        assert_eq!(from("home/toy", "docs/../c"), path("home/toy/a/c"), "`..` of the link's target, not of `docs`");
        assert_eq!(from("home/toy/a", ".."), path("home/toy"));
        assert_eq!(from("home", ".."), Err(Refusal::Escape));
        assert_eq!(from("home/toy", "../.."), Err(Refusal::Escape));
        assert_eq!(from("home/toy", "../../home/toy"), Err(Refusal::Escape), "an escape that would come back is refused");
    }

    /// `rel` from the directory a connection holds at `held`, after another
    /// connection renamed `from` to `to` between its two requests, as the
    /// server moves a held directory (`descriptions::renamed`).
    fn after_rename(root: &str, held: &str, (from, to): (&str, &str), links: &[(&str, &str)], rel: &str) -> Result<Resolved, Refusal> {
        let (mut start, mut removed) = (held.to_string(), false);
        crate::descriptions::renamed(&mut start, &mut removed, from, to);
        assert!(!removed, "{held} after {from} -> {to}");
        let links = vol(links);
        resolve(root, &start, rel, true, &mut |path| Ok(links.get(path).map_or(Found::Other, |t| Found::Link(t.clone()))))
    }

    /// The escape suite's second half: a rename racing a lookup. A directory
    /// the connection holds open, moved out of its root by a rename it did not
    /// make, names nothing from there: not by a plain name, not by `..`, and
    /// not when only its spelling still begins with the root's.
    #[test]
    fn a_held_directory_renamed_out_of_the_root_names_nothing() {
        let root = "home/toy";
        for (from, to) in [
            ("home/toy/a", "home/other/a"),
            ("home/toy/a", "state/sshserver/a"),
            ("home/toy/a", "home/toy2/a"),
            ("home/toy/a", "a"),
        ] {
            for rel in ["", "x", "..", "../..", "b/../c"] {
                assert_eq!(after_rename(root, "home/toy/a", (from, to), &[], rel), Err(Refusal::Escape), "{from} -> {to}, then {rel:?}");
            }
        }
    }

    /// The root itself renamed away under a held directory: the directory
    /// moved with it, and the grant still names the old root, so it is
    /// outside the root it started under.
    #[test]
    fn a_held_directory_whose_root_was_renamed_names_nothing() {
        let rename = ("home/toy/Games", "home/toy/Old");
        for rel in ["", "a.gba", "..", "../Games"] {
            assert_eq!(after_rename("home/toy/Games", "home/toy/Games/GBA", rename, &[], rel), Err(Refusal::Escape), "{rel:?}");
        }
    }

    /// A rename inside the root moves the held directory with it, and a link
    /// planted at its old name changes nothing the description names: `..`
    /// climbs from where it is now and stops at the root, and the planted link
    /// is refused by a path that goes through it.
    #[test]
    fn a_held_directory_renamed_inside_the_root_resolves_where_it_now_is() {
        let root = "home/toy";
        let planted = [("home/toy/a", "../../state")];
        let moved = ("home/toy/a", "home/toy/b/a");
        assert_eq!(after_rename(root, "home/toy/a", moved, &planted, "f"), path("home/toy/b/a/f"));
        assert_eq!(after_rename(root, "home/toy/a", moved, &planted, "../.."), path("home/toy"));
        assert_eq!(after_rename(root, "home/toy/a", moved, &planted, "../../.."), Err(Refusal::Escape));
        assert_eq!(after_rename(root, "home/toy", moved, &planted, "a/sshserver"), Err(Refusal::Escape));
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
