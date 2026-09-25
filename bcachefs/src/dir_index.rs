//! The directories this volume's flat namespace implies, answered in path depth.
//!
//! Every entry is keyed by a hash of its whole name, so the tree cannot say
//! whether any name lies beneath `a/b` short of reading all of it. This trie of
//! the names' components can: a directory is a component with something
//! beneath it, and nothing else is one. It holds every name the volume answers
//! to, is built by the one walk a mount makes, and is kept by each call that
//! adds or removes a name — so its memory is the names' bytes, bounded by the
//! volume that stored them.

use alloc::collections::BTreeMap;
use alloc::string::String;

#[derive(Default)]
pub(crate) struct DirIndex {
    root: Node,
}

#[derive(Default)]
struct Node {
    children: BTreeMap<String, Node>,
    /// A name ends here; independent of `children`, since the flat namespace
    /// lets `a` and `a/b` both be files.
    named: bool,
}

impl DirIndex {
    /// Record `name`; recording it twice is recording it once.
    pub(crate) fn add(&mut self, name: &str) {
        let mut node = &mut self.root;
        for component in name.split('/') {
            node = node.children.entry(String::from(component)).or_default();
        }
        node.named = true;
    }

    /// Forget `name`, and every directory only it kept alive.
    pub(crate) fn remove(&mut self, name: &str) {
        fn remove_below<'a>(node: &mut Node, mut components: impl Iterator<Item = &'a str>) {
            let Some(component) = components.next() else {
                node.named = false;
                return;
            };
            let Some(child) = node.children.get_mut(component) else { return };
            remove_below(child, components);
            if !child.named && child.children.is_empty() {
                node.children.remove(component);
            }
        }
        remove_below(&mut self.root, name.split('/'));
    }

    /// Whether some name lies beneath `dir`; `""` is the volume's root, always one.
    pub(crate) fn is_dir(&self, dir: &str) -> bool {
        if dir.is_empty() {
            return true;
        }
        let mut node = &self.root;
        for component in dir.split('/') {
            match node.children.get(component) {
                Some(child) => node = child,
                None => return false,
            }
        }
        !node.children.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::DirIndex;

    #[test]
    fn a_directory_is_a_name_with_something_beneath_it() {
        let mut dirs = DirIndex::default();
        dirs.add("a/b/file");
        dirs.add("a/leaf");
        assert!(dirs.is_dir(""));
        assert!(dirs.is_dir("a"));
        assert!(dirs.is_dir("a/b"));
        assert!(!dirs.is_dir("a/b/file"), "a file is not a directory");
        assert!(!dirs.is_dir("a/leaf"));
        assert!(!dirs.is_dir("a/missing"));
        assert!(!dirs.is_dir("b"));
        assert!(!dirs.is_dir("a/b/file/deeper"));
        assert!(!dirs.is_dir("a/"), "an empty component names nothing recorded");
    }

    #[test]
    fn a_removed_name_takes_only_the_directories_it_alone_kept() {
        let mut dirs = DirIndex::default();
        dirs.add("a/b/one");
        dirs.add("a/two");
        dirs.remove("a/b/one");
        assert!(!dirs.is_dir("a/b"));
        assert!(dirs.is_dir("a"), "a/two still lies beneath a");
        dirs.remove("a/two");
        assert!(!dirs.is_dir("a"));
    }

    #[test]
    fn a_name_that_is_also_a_directory_stays_one_when_its_file_goes() {
        let mut dirs = DirIndex::default();
        dirs.add("a");
        dirs.add("a/b");
        dirs.remove("a");
        assert!(dirs.is_dir("a"));
        dirs.remove("a/b");
        assert!(!dirs.is_dir("a"));
    }

    #[test]
    fn adding_twice_and_removing_the_absent_change_nothing() {
        let mut dirs = DirIndex::default();
        dirs.add("a/b");
        dirs.add("a/b");
        dirs.remove("x/y");
        dirs.remove("a/b/c");
        assert!(dirs.is_dir("a"));
        dirs.remove("a/b");
        assert!(!dirs.is_dir("a"));
    }
}
