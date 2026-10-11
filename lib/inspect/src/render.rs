//! The two renderings: `path = value` lines for a person and a pipe, and one
//! JSON object for a program.

use alloc::string::String;
use core::fmt::Write;

use crate::wire::Value;

/// One line, without its newline: `net.link.state = up`.
///
/// **No quoting, no escaping and no units**: the line is for `grep` and `cut`,
/// and a value cannot hold a control character (`crate::decode` refuses one),
/// so the first ` = ` always ends the path.
pub fn line(path: &str, value: &Value) -> String {
    alloc::format!("{path} = {value}")
}

/// Every entry as one flat JSON object keyed by path, in the order given, with
/// a trailing newline. Numbers are JSON numbers, bools are JSON bools and text
/// is a JSON string.
pub fn json<'a>(entries: impl IntoIterator<Item = (&'a str, &'a Value)>) -> String {
    let mut out = String::from("{");
    for (i, (path, value)) in entries.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        string(&mut out, path);
        out.push(':');
        match value {
            Value::U64(v) => write!(out, "{v}").expect("a String takes a write"),
            Value::Bool(v) => write!(out, "{v}").expect("a String takes a write"),
            Value::Text(v) => string(&mut out, v),
        }
    }
    out.push_str("}\n");
    out
}

/// A JSON string. A path is `a-z0-9_-:.` and a text value holds no control
/// character, so the quote and the backslash are the only two that need it.
fn string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => {
                debug_assert!(!c.is_control(), "a decoded value holds no control character");
                out.push(c);
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_path_equals_value() {
        assert_eq!(line("net.link.state", &Value::Text("up".into())), "net.link.state = up");
        assert_eq!(line("net.errors.crc", &Value::U64(3)), "net.errors.crc = 3");
        assert_eq!(line("net.link.full_duplex", &Value::Bool(false)), "net.link.full_duplex = false");
    }

    #[test]
    fn json_is_one_flat_object_with_typed_values() {
        let text = Value::Text("a \"b\" \\c".into());
        let n = Value::U64(u64::MAX);
        let t = Value::Bool(true);
        assert_eq!(
            json([("log.volume.path", &text), ("log.volume.bytes", &n), ("log.stream", &t)]),
            "{\"log.volume.path\":\"a \\\"b\\\" \\\\c\",\"log.volume.bytes\":18446744073709551615,\
             \"log.stream\":true}\n"
        );
        assert_eq!(json([]), "{}\n");
    }
}
