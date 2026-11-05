//! A set's shape: which libraries a load lays out, and the order their
//! constructors run in.

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::{discover, order, Refused};

/// Who names whom: each library and the libraries it names.
const NAMES: &[(&str, &[&str])] = &[
    ("app", &["a", "b"]),
    ("a", &["c", "b"]),
    ("b", &["c"]),
    ("c", &["a"]),
    ("d", &["d"]),
];

/// The set `roots` loads with `published` already in the list: each name in
/// the order it was laid, and every name laid in that order.
fn set(roots: &[&str], published: &[&str]) -> (Vec<String>, Vec<String>) {
    let mut laid = Vec::new();
    let set = discover(roots.iter().map(|r| r.to_string()), |name| published.contains(&name), |name| {
        laid.push(name.to_string());
        let needed = NAMES.iter().find(|(n, _)| *n == name).unwrap().1;
        Ok(((), needed.iter().map(|n| n.to_string()).collect()))
    })
    .unwrap();
    (set.into_iter().map(|node| node.name).collect(), laid)
}

/// A cycle of names is one set, each library laid once, breadth first: a
/// recursion over `DT_NEEDED` would never end.
#[test]
fn a_cycle_of_names_is_laid_once_breadth_first() {
    let (names, laid) = set(&["app"], &[]);
    assert_eq!(names, ["app", "a", "b", "c"]);
    assert_eq!(laid, names);
    assert_eq!(set(&["d"], &[]).0, ["d"]);
}

/// A library already in the list is bound to, not laid again.
#[test]
fn a_published_library_is_not_laid_again() {
    assert_eq!(set(&["app"], &["b"]).0, ["app", "a", "c"]);
    assert!(set(&["b"], &["b"]).0.is_empty());
}

/// The first refusal ends the walk: nothing after it is laid.
#[test]
fn a_refusal_ends_the_set() {
    let mut laid = Vec::new();
    let refused = discover(["app".to_string()], |_| false, |name| {
        laid.push(name.to_string());
        match name {
            "a" => Err(Refused::Elf("refused")),
            _ => Ok(((), Vec::from(["a".to_string(), "b".to_string()]))),
        }
    });
    assert!(matches!(refused, Err(Refused::Elf("refused"))));
    assert_eq!(laid, ["app", "a"]);
}

/// A library's constructors run after those of every library of the set it
/// names, transitively, each once; within a cycle, after those it reached
/// first.
#[test]
fn constructors_run_after_those_of_the_libraries_named() {
    let set = discover(["app".to_string()], |_| false, |name| {
        let needed = NAMES.iter().find(|(n, _)| *n == name).unwrap().1;
        Ok(((), needed.iter().map(|n| n.to_string()).collect()))
    })
    .unwrap();
    let names: Vec<&str> = order(&set).into_iter().map(|i| set[i].name.as_str()).collect();
    // app names a, which names c, which names a (already seen): c, then a's
    // other name b, whose c is done, then a, then app.
    assert_eq!(names, ["c", "b", "a", "app"]);
}

/// A `DT_NEEDED` name is a path, or a file name in the system's library
/// directory; an empty one, which an unreadable name also reads as, names
/// nothing.
#[test]
fn a_needed_name_is_a_path_or_a_system_library() {
    assert_eq!(super::path_of("libx.so").unwrap(), "/system/lib/libx.so");
    assert_eq!(super::path_of("/app/lib/libx.so").unwrap(), "/app/lib/libx.so");
    assert!(matches!(super::path_of(""), Err(Refused::Elf("ELF: a DT_NEEDED names no library"))));
}
