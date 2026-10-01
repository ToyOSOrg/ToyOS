//! A workflow under `.github/workflows/`, read as GitHub reads it: YAML, by a
//! parser, so a gate holds the structure that runs and no spelling of it.
//!
//! An alias is resolved as GitHub resolves it. The node it makes is marked, as
//! is every node an anchor or a tag names, and [`Node::mark`] finds them. A
//! key given twice, a key that is not a plain scalar, and a file that is not
//! one document are refused.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::Marker;

/// One node of a workflow, and the line it starts on.
#[derive(Clone, Debug)]
pub struct Node {
    value: Value,
    pub line: usize,
    marked: bool,
}

#[derive(Clone, Debug)]
enum Value {
    Scalar(String),
    Seq(Vec<Node>),
    Map(Vec<(String, Node)>),
}

impl Node {
    /// The value under `key`, or `None` where this mapping has no such key.
    pub fn get(&self, key: &str) -> Result<Option<&Node>, String> {
        Ok(self.map()?.iter().find(|(k, _)| k == key).map(|(_, node)| node))
    }

    pub fn map(&self) -> Result<&[(String, Node)], String> {
        match &self.value {
            Value::Map(entries) => Ok(entries),
            _ => Err(format!("line {}: not a mapping", self.line)),
        }
    }

    pub fn seq(&self) -> Result<&[Node], String> {
        match &self.value {
            Value::Seq(items) => Ok(items),
            _ => Err(format!("line {}: not a sequence", self.line)),
        }
    }

    pub fn str(&self) -> Result<&str, String> {
        match &self.value {
            Value::Scalar(text) => Ok(text),
            _ => Err(format!("line {}: not a scalar", self.line)),
        }
    }

    /// The line of the first node at or under this one that an anchor, an
    /// alias or a tag names or made.
    pub fn mark(&self) -> Option<usize> {
        if self.marked {
            return Some(self.line);
        }
        match &self.value {
            Value::Scalar(_) => None,
            Value::Seq(items) => items.iter().find_map(Node::mark),
            Value::Map(entries) => entries.iter().find_map(|(_, node)| node.mark()),
        }
    }
}

/// Every workflow, by file name in name order: each `.yml` and `.yaml` file
/// in `root`'s `.github/workflows`, in any case, a superset of the files
/// GitHub reads.
pub fn all(root: &Path) -> Result<Vec<(String, Node)>, String> {
    let dir = root.join(".github/workflows");
    let mut found = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let lower = name.to_ascii_lowercase();
        if !(lower.ends_with(".yml") || lower.ends_with(".yaml")) {
            continue;
        }
        let text = fs::read_to_string(entry.path()).map_err(|e| format!("read {name}: {e}"))?;
        found.push((name.clone(), parse(&text).map_err(|e| format!("{name}: {e}"))?));
    }
    found.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(found)
}

/// One YAML document.
fn parse(text: &str) -> Result<Node, String> {
    let mut tree = Tree::default();
    Parser::new_from_str(text).load(&mut tree, true).map_err(|e| e.to_string())?;
    if let Some(refusal) = tree.refusal {
        return Err(refusal);
    }
    let count = tree.docs.len();
    let [doc] = <[Node; 1]>::try_from(tree.docs).map_err(|_| format!("{count} documents, not one"))?;
    Ok(doc)
}

/// The parser's events, built: the documents, each collection still open with
/// its anchor and, in a mapping, the key whose value comes next, and each
/// anchored node by its anchor.
#[derive(Default)]
struct Tree {
    docs: Vec<Node>,
    open: Vec<(Node, usize, Option<String>)>,
    anchors: HashMap<usize, Node>,
    refusal: Option<String>,
}

impl MarkedEventReceiver for Tree {
    fn on_event(&mut self, event: Event, mark: Marker) {
        if self.refusal.is_none() {
            if let Err(why) = self.take(event, mark.line()) {
                self.refusal = Some(format!("line {}: {why}", mark.line()));
            }
        }
    }
}

impl Tree {
    fn take(&mut self, event: Event, line: usize) -> Result<(), String> {
        let node = |value, anchor: usize, tagged: bool| Node { value, line, marked: anchor > 0 || tagged };
        match event {
            Event::SequenceStart(anchor, tag) => {
                self.open.push((node(Value::Seq(Vec::new()), anchor, tag.is_some()), anchor, None));
            }
            Event::MappingStart(anchor, tag) => {
                self.open.push((node(Value::Map(Vec::new()), anchor, tag.is_some()), anchor, None));
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let (done, anchor, _) = self.open.pop().expect("the parser ends only what it began");
                self.place(done, anchor)?;
            }
            Event::Scalar(text, _, anchor, tag) => self.place(node(Value::Scalar(text), anchor, tag.is_some()), anchor)?,
            // The copy of a node its anchor marked, so marked too.
            Event::Alias(anchor) => {
                let mut copy = self.anchors.get(&anchor).ok_or("an alias inside the node it names")?.clone();
                copy.line = line;
                self.place(copy, 0)?;
            }
            Event::Nothing | Event::StreamStart | Event::StreamEnd | Event::DocumentStart | Event::DocumentEnd => {}
        }
        Ok(())
    }

    /// `node` into the collection open last, or as a document.
    fn place(&mut self, node: Node, anchor: usize) -> Result<(), String> {
        if anchor > 0 {
            self.anchors.insert(anchor, node.clone());
        }
        let Some((parent, _, key)) = self.open.last_mut() else {
            self.docs.push(node);
            return Ok(());
        };
        match (&mut parent.value, key.take()) {
            (Value::Seq(items), _) => items.push(node),
            (Value::Map(entries), Some(key)) => {
                if entries.iter().any(|(k, _)| *k == key) {
                    return Err(format!("`{key}` twice in one mapping"));
                }
                entries.push((key, node));
            }
            (Value::Map(_), None) => match node {
                Node { value: Value::Scalar(text), marked: false, .. } => *key = Some(text),
                _ => return Err("a key that is not a plain scalar".into()),
            },
            (Value::Scalar(_), _) => unreachable!("only a collection is open"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What GitHub reads is what the reader holds: a comment, a quoted key and a
    /// folded line are spellings, an alias is its anchor's node, and every
    /// node an anchor, an alias or a tag made is found.
    #[test]
    fn a_workflow_is_its_structure_and_not_its_spelling() {
        let doc = parse(
            "jobs: # a comment
  \"host\":
    steps: &steps
      - run: cargo run
          || true
  again:
    steps: *steps
  tagged:
    run: !!str x
",
        )
        .unwrap();
        let jobs = doc.get("jobs").unwrap().unwrap();
        assert_eq!(jobs.map().unwrap().len(), 3);
        let run = |job: &str| {
            let steps = jobs.get(job).unwrap().unwrap().get("steps").unwrap().unwrap();
            steps.seq().unwrap()[0].get("run").unwrap().unwrap().str().unwrap().to_string()
        };
        assert_eq!([run("host"), run("again")], ["cargo run || true", "cargo run || true"]);
        let mark = |job: &str| jobs.get(job).unwrap().unwrap().mark();
        assert_eq!([mark("host"), mark("again"), mark("tagged")], [Some(4), Some(7), Some(9)]);
    }

    #[test]
    fn a_document_the_reader_cannot_hold_is_refused() {
        for (text, refusal) in [
            ("a: 1\n\"a\": 2\n", "line 2: `a` twice in one mapping"),
            ("&k a: 1\n", "line 1: a key that is not a plain scalar"),
            ("? [a]\n: 1\n", "a key that is not a plain scalar"),
            ("a: &x [*x]\n", "an alias inside the node it names"),
            ("a: 1\n---\nb: 2\n", "2 documents, not one"),
            ("a: *x\n", "unknown anchor"),
        ] {
            let said = parse(text).expect_err(text);
            assert!(said.contains(refusal), "{text:?}: {said}");
        }
    }
}
