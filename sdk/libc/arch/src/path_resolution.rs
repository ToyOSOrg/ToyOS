//! Canonicalization through a process's capability namespace, including its
//! synthetic ancestors. No second resolution policy handles missing names.
use std::collections::BTreeMap;
use toyos_abi::syscall::MAX_PATH;
use toyos::files::MAX_LINKS;
use crate::errcode::{EIO, ELOOP, ENAMETOOLONG, ENOENT, ENOTDIR};
use crate::header::{self, LIMITS_H};
use crate::resolve::{resolve_path, Name, Served};

#[derive(Clone, Copy)]
enum Node { File, Dir, Link(&'static str), Broken }
struct Tree(BTreeMap<String, Node>);
impl Tree {
    fn new(nodes: &[(&str, Node)]) -> Self {
        Self(nodes.iter().map(|&(path, node)| (String::from(path), node)).collect())
    }
}
impl Served for Tree {
    fn name(&mut self, at: &[u8]) -> Result<Name, i32> {
        match self.0.get(std::str::from_utf8(at).unwrap()) {
            Some(Node::File) => Ok(Name::Other),
            Some(Node::Dir) => Ok(Name::Directory),
            Some(Node::Link(target)) => Ok(Name::Link(target.as_bytes().to_vec())),
            Some(Node::Broken) => Err(EIO),
            None => Err(ENOENT),
        }
    }
}
fn tree() -> Tree {
    Tree::new(&[
        ("/home", Node::Dir), ("/home/u", Node::Dir),
        ("/home/u/a", Node::Dir), ("/home/u/a/b", Node::Dir),
        ("/home/u/a/f", Node::File), ("/home/u/f", Node::File),
        ("/home/u/rel", Node::Link("a/f")),
        ("/home/u/abs", Node::Link("/home/u/a/f")),
        ("/home/u/d", Node::Link("a")), ("/home/u/deep", Node::Link("a/b")),
        ("/home/u/out", Node::Link("/tmp/f")), ("/home/u/denied", Node::Link("/system/private")),
        ("/home/u/x", Node::Link("y")), ("/home/u/y", Node::Link("x")),
        ("/home/u/gone", Node::Link("absent")), ("/home/u/broken", Node::Broken),
        ("/tmp", Node::Dir), ("/tmp/f", Node::File),
    ])
}
fn answer(cwd: &str, path: &str) -> Result<String, i32> {
    resolve_path(cwd.as_bytes(), path.as_bytes(), &mut tree()).map(|at| String::from_utf8(at).unwrap())
}
#[test]
fn links_are_followed_at_each_component_and_relative_to_their_directory() {
    assert_eq!(answer("/", "/home/u/rel"), Ok("/home/u/a/f".into()));
    assert_eq!(answer("/", "/home/u/abs"), Ok("/home/u/a/f".into()));
    assert_eq!(answer("/home/u", "./a//f"), Ok("/home/u/a/f".into()));
    assert_eq!(answer("/", "/home/u/d/f"), Ok("/home/u/a/f".into()));
    assert_eq!(answer("/", "/home/u/deep/../f"), Ok("/home/u/a/f".into()));
    assert_eq!(answer("/", "/home/u/a/../f"), Ok("/home/u/f".into()));
}
#[test]
fn absolute_links_reenter_the_same_capability_namespace() {
    assert_eq!(answer("/", "/home/u/out"), Ok("/tmp/f".into()));
    assert_eq!(answer("/", "/home/u/denied"), Err(ENOENT));
    assert_eq!(answer("/home/u", "../../tmp/f"), Ok("/tmp/f".into()));
}
#[test]
fn missing_names_files_as_parents_and_refusals_are_preserved() {
    assert_eq!(answer("/", "/home/u/gone"), Err(ENOENT));
    assert_eq!(answer("/", "/home/u/a/f/g"), Err(ENOTDIR));
    assert_eq!(answer("/", "/home/u/a/f/.."), Err(ENOTDIR));
    assert_eq!(answer("/", "/home/u/missing/../f"), Err(ENOENT));
    assert_eq!(answer("/", "/home/u/broken"), Err(EIO));
    assert_eq!(answer("/", ""), Err(ENOENT));
    assert_eq!(answer("/", "/"), Ok("/".into()));
}
#[test]
fn links_are_bounded_even_across_directories() {
    assert_eq!(answer("/", "/home/u/x"), Err(ELOOP));
    for (links, expected) in [(MAX_LINKS, Ok(b"/tmp/f".to_vec())), (MAX_LINKS + 1, Err(ELOOP))] {
        let mut t = tree();
        for i in 0..links {
            let target = if i + 1 == links { "/tmp/f".into() } else { format!("l{}", i + 1) };
            t.0.insert(format!("/tmp/l{i}"), Node::Link(Box::leak(target.into_boxed_str())));
        }
        assert_eq!(resolve_path(b"/", b"/tmp/l0", &mut t), expected);
    }
}
#[test]
fn paths_and_link_targets_are_bounded() {
    let long = format!("/tmp/{}", "a".repeat(MAX_PATH));
    assert_eq!(answer("/", &long), Err(ENAMETOOLONG));
    let name = "b".repeat(MAX_PATH - 6);
    assert_eq!(answer("/home/u", &name), Err(ENAMETOOLONG));
    let mut t = tree();
    t.0.insert("/tmp/long".into(), Node::Link(Box::leak(long.into_boxed_str())));
    assert_eq!(resolve_path(b"/", b"/tmp/long", &mut t), Err(ENAMETOOLONG));
    assert_eq!(MAX_PATH, header::int(LIMITS_H, "PATH_MAX") as usize);
}
