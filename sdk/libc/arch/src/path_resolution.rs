//! `realpath`'s resolution (`resolve.rs`) over a tree laid out here and asked
//! as the kernel answers it: `..` read off the text, only the last name's link
//! followed, a relative link read against its mount, a chain of links past
//! `MAX_LINKS` refused `ELOOP` as a loop is, and a refusal's errno.

use std::collections::BTreeMap;

use toyos_abi::syscall::{SyscallError, MAX_LINKS, MAX_PATH};

use crate::errcode::{EIO, ELOOP, ENAMETOOLONG, ENOENT, ENOTDIR};
use crate::header::{self, LIMITS_H};
use crate::resolve::{resolve, resolve_path, Kernel, Name, Served};

#[derive(Clone, Copy)]
enum Node {
    File,
    Dir,
    Link(&'static str),
    /// A name every call about refuses `Io`, as a device that would not answer.
    Broken,
}

/// A tree keyed by absolute path, answering as `kernel/src/vfs.rs` does:
/// `open` follows a last-name link, `readdir` and `readlink` follow none.
struct Tree(BTreeMap<String, Node>);

impl Tree {
    fn new(nodes: &[(&str, Node)]) -> Self {
        Tree(nodes.iter().map(|&(path, node)| (String::from(path), node)).collect())
    }

    fn at(&self, at: &[u8]) -> Option<&Node> {
        self.0.get(std::str::from_utf8(at).unwrap())
    }
}

impl Kernel for Tree {
    fn read_link(&mut self, at: &[u8], target: &mut [u8]) -> Result<usize, SyscallError> {
        match self.at(at) {
            Some(&Node::Link(t)) => {
                let len = t.len().min(target.len());
                target[..len].copy_from_slice(&t.as_bytes()[..len]);
                Ok(len)
            }
            Some(Node::Broken) => Err(SyscallError::Io),
            _ => Err(SyscallError::NotFound),
        }
    }

    fn open(&mut self, at: &[u8]) -> Result<(), SyscallError> {
        match self.at(at) {
            Some(Node::File) => Ok(()),
            Some(&Node::Link(t)) if t.starts_with('/') => self.open(t.as_bytes()),
            Some(Node::Broken) => Err(SyscallError::Io),
            _ => Err(SyscallError::NotFound),
        }
    }

    fn is_dir(&mut self, at: &[u8]) -> bool {
        at == b"/" || matches!(self.at(at), Some(Node::Dir))
    }
}

/// The tree every case reads: `/tmp` a mount, `/tmp/d` a directory in it.
fn tree() -> Tree {
    Tree::new(&[
        ("/tmp", Node::Dir),
        ("/tmp/d", Node::Dir),
        ("/tmp/d/f", Node::File),
        ("/tmp/x", Node::Dir),
        ("/tmp/x/f", Node::File),
        ("/tmp/d/abs", Node::Link("/tmp/x/f")),
        ("/tmp/d/rel", Node::Link("x/f")),
        ("/tmp/d/up", Node::Link("../system/bin/toybox")),
        ("/tmp/d/chain", Node::Link("/tmp/d/abs")),
        ("/tmp/d/dir", Node::Link("/tmp/x")),
        ("/tmp/d/dangling", Node::Link("/tmp/none")),
        ("/tmp/d/self", Node::Link("/tmp/d/self")),
        ("/tmp/d/ping", Node::Link("/tmp/d/pong")),
        ("/tmp/d/pong", Node::Link("d/ping")),
        ("/tmp/d/broken", Node::Broken),
        ("/system", Node::Dir),
        ("/system/bin", Node::Dir),
        ("/system/bin/toybox", Node::File),
    ])
}

fn answer(cwd: &str, path: &str) -> Result<String, i32> {
    resolve(cwd.as_bytes(), path.as_bytes(), &mut tree()).map(|at| String::from_utf8(at).unwrap())
}

#[test]
fn a_relative_path_is_read_against_the_working_directory() {
    assert_eq!(answer("/tmp/d", "f"), Ok("/tmp/d/f".into()));
    assert_eq!(answer("/tmp", "./d//f"), Ok("/tmp/d/f".into()));
    assert_eq!(answer("/", "tmp/d"), Ok("/tmp/d".into()));
}

#[test]
fn dot_dot_is_read_off_the_text_before_any_name_is_looked_up() {
    assert_eq!(answer("/tmp/d", "../x/f"), Ok("/tmp/x/f".into()));
    // `dir` is a link to `/tmp/x`, whose parent is not `/tmp/d`'s.
    assert_eq!(answer("/", "/tmp/d/dir/../f"), Ok("/tmp/d/f".into()));
    assert_eq!(answer("/tmp", "../../../.."), Ok("/".into()));
    // `..` takes off a name that does not exist without asking.
    assert_eq!(answer("/", "/tmp/d/none/../f"), Ok("/tmp/d/f".into()));
}

#[test]
fn the_last_names_links_are_followed_to_the_file() {
    assert_eq!(answer("/", "/tmp/d/abs"), Ok("/tmp/x/f".into()));
    assert_eq!(answer("/tmp/d", "chain"), Ok("/tmp/x/f".into()));
    assert_eq!(answer("/", "/tmp/d/dir"), Ok("/tmp/x".into()));
}

#[test]
fn a_relative_link_is_read_against_its_mount_not_its_directory() {
    // `/tmp/d/x/f` is not there; `/tmp/x/f` is.
    assert_eq!(answer("/", "/tmp/d/rel"), Ok("/tmp/x/f".into()));
    // Its `..` leaves the mount, as the kernel's does.
    assert_eq!(answer("/", "/tmp/d/up"), Ok("/system/bin/toybox".into()));
}

#[test]
fn a_link_before_the_last_name_is_not_followed() {
    // The kernel looks `/tmp/d/dir/f` up as that name, and there is none.
    assert_eq!(answer("/", "/tmp/d/dir/f"), Err(ENOENT));
}

#[test]
fn a_loop_of_links_is_eloop() {
    assert_eq!(answer("/", "/tmp/d/self"), Err(ELOOP));
    assert_eq!(answer("/", "/tmp/d/ping"), Err(ELOOP));
}

/// `/tmp/l0`, a chain of `links` links, each relative, ending in `/tmp/end`.
fn chain(links: u32) -> Tree {
    let mut nodes: Vec<(&'static str, Node)> = vec![("/tmp", Node::Dir), ("/tmp/end", Node::File)];
    for i in 0..links {
        let target = if i + 1 == links { "/tmp/end".to_string() } else { format!("l{}", i + 1) };
        nodes.push((leak(format!("/tmp/l{i}")), Node::Link(leak(target))));
    }
    Tree::new(&nodes)
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

#[test]
fn a_chain_of_max_links_is_followed_and_one_more_is_eloop() {
    let at = resolve(b"/", b"/tmp/l0", &mut chain(MAX_LINKS)).map(|at| String::from_utf8(at).unwrap());
    assert_eq!(at, Ok("/tmp/end".into()));
    assert_eq!(resolve(b"/", b"/tmp/l0", &mut chain(MAX_LINKS + 1)), Err(ELOOP));
}

#[test]
fn a_name_that_is_not_there_is_enoent() {
    assert_eq!(answer("/", "/tmp/d/none"), Err(ENOENT));
    assert_eq!(answer("/", "/tmp/none/f"), Err(ENOENT));
    assert_eq!(answer("/", "/tmp/d/dangling"), Err(ENOENT));
    assert_eq!(answer("/tmp", ""), Err(ENOENT));
}

#[test]
fn a_file_passed_through_as_a_directory_is_enotdir() {
    assert_eq!(answer("/", "/tmp/d/f/g"), Err(ENOTDIR));
    // A link to a file, as POSIX has it.
    assert_eq!(answer("/", "/tmp/d/abs/g/h"), Err(ENOTDIR));
}

#[test]
fn a_path_or_an_answer_past_path_max_is_enametoolong() {
    let long = format!("/tmp/{}", "a".repeat(MAX_PATH));
    assert_eq!(answer("/", &long), Err(ENAMETOOLONG));
    // Short enough as written, too long once read against the directory.
    let name = "b".repeat(MAX_PATH - 6);
    assert_eq!(answer("/tmp/d", &name), Err(ENAMETOOLONG));
    // A working directory and a path joined past it, which `..` shortens.
    let deep = format!("/tmp/{}", "c".repeat(MAX_PATH - 10));
    assert_eq!(answer(&deep, "../d/f"), Ok("/tmp/d/f".into()));
}

#[test]
fn a_link_target_that_fills_the_buffer_is_enametoolong() {
    let target: &'static str = Box::leak(format!("/{}", "t".repeat(MAX_PATH)).into_boxed_str());
    let mut tree = Tree::new(&[("/tmp", Node::Dir), ("/tmp/long", Node::Link(target))]);
    assert_eq!(resolve(b"/", b"/tmp/long", &mut tree), Err(ENAMETOOLONG));
}

#[test]
fn a_kernel_refusal_is_its_errno() {
    assert_eq!(answer("/", "/tmp/d/broken"), Err(EIO));
}

#[test]
fn path_max_is_limits_h_s() {
    assert_eq!(MAX_PATH, header::int(LIMITS_H, "PATH_MAX") as usize);
}

/// A file server holding `/home`, its names in `home`, beside the kernel's
/// [`tree`]: as `toyos::files` answers, a name is its link's target or
/// nothing, one that is not there `ENOENT`, and one through a file `ENOTDIR`.
struct Home(Tree);

impl Served for Home {
    fn holds(&mut self, at: &[u8]) -> bool {
        at == b"/home" || at.starts_with(b"/home/")
    }

    fn name(&mut self, at: &[u8]) -> Result<Name, i32> {
        let parent = &at[..at.iter().rposition(|&b| b == b'/').unwrap()];
        if parent.len() > b"/home".len() && matches!(self.0.at(parent), Some(Node::File)) {
            return Err(ENOTDIR);
        }
        match self.0.at(at) {
            Some(Node::Link(target)) => Ok(Name::Link(target.as_bytes().to_vec())),
            Some(_) => Ok(Name::Other),
            None if at == b"/home" => Ok(Name::Other),
            None => Err(ENOENT),
        }
    }
}

fn home() -> Home {
    Home(Tree::new(&[
        ("/home/u", Node::Dir),
        ("/home/u/a", Node::Dir),
        ("/home/u/a/b", Node::Dir),
        ("/home/u/a/file", Node::File),
        ("/home/u/a/f", Node::File),
        ("/home/u/rel", Node::Link("a/file")),
        ("/home/u/abs", Node::Link("/home/u/a/file")),
        ("/home/u/d", Node::Link("a")),
        ("/home/u/deep", Node::Link("a/b")),
        ("/home/u/out", Node::Link("/tmp/d/f")),
        ("/home/u/outdir", Node::Link("/tmp/x")),
        ("/home/u/x", Node::Link("y")),
        ("/home/u/y", Node::Link("x")),
        ("/home/u/dangling", Node::Link("gone")),
    ]))
}

fn mixed(cwd: &str, path: &str) -> Result<String, i32> {
    resolve_path(cwd.as_bytes(), path.as_bytes(), &mut home(), &mut tree()).map(|at| String::from_utf8(at).unwrap())
}

/// A served link is read against its own directory, as POSIX has it, and
/// not against a mount as the kernel's is; an absolute one from the root.
#[test]
fn a_served_link_is_read_against_its_own_directory() {
    assert_eq!(mixed("/", "/home/u/rel"), Ok("/home/u/a/file".into()));
    assert_eq!(mixed("/", "/home/u/abs"), Ok("/home/u/a/file".into()));
    assert_eq!(mixed("/home/u", "./a//file"), Ok("/home/u/a/file".into()), "relative and lexically clean");
}

/// Every name's link is followed where it is met, so `..` after one is the
/// parent of where it led, and two spellings of one file answer one path.
#[test]
fn a_served_link_before_the_last_name_is_followed_and_dot_dot_after_it_is_its_parent() {
    assert_eq!(mixed("/", "/home/u/d/file"), Ok("/home/u/a/file".into()));
    assert_eq!(mixed("/", "/home/u/deep/../f"), Ok("/home/u/a/f".into()), "the parent of a/b, not of deep");
    assert_eq!(mixed("/", "/home/u/d/../d/file"), mixed("/", "/home/u/a/file"));
}

/// A served link to a path the kernel holds is resolved there, by the
/// kernel's rules, whether it is the last name or one before it.
#[test]
fn a_served_link_out_to_the_kernel_is_resolved_by_the_kernel() {
    assert_eq!(mixed("/", "/home/u/out"), Ok("/tmp/d/f".into()));
    assert_eq!(mixed("/", "/home/u/outdir/f"), Ok("/tmp/x/f".into()));
    assert_eq!(mixed("/", "/tmp/d/abs"), Ok("/tmp/x/f".into()), "a kernel path is the kernel's from its first name");
    assert_eq!(mixed("/home/u", "../../tmp/d/f"), Ok("/tmp/d/f".into()));
}

#[test]
fn a_served_loop_is_eloop_and_a_missing_name_enoent_or_enotdir() {
    assert_eq!(mixed("/", "/home/u/x"), Err(ELOOP));
    assert_eq!(mixed("/", "/home/u/dangling"), Err(ENOENT));
    assert_eq!(mixed("/", "/home/u/a/file/g"), Err(ENOTDIR));
    assert_eq!(mixed("/", "/"), Ok("/".into()));
}
