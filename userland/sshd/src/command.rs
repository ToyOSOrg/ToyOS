//! What an `exec` channel request means: a program and its arguments, and
//! nothing else.
//!
//! **SSH has no argument vector.** `SSH_MSG_CHANNEL_REQUEST "exec"` carries one
//! string, and every client builds it by pasting words together — the remote
//! side is expected to be a shell. This daemon is not one, so the split is
//! spelled here and its whole grammar is quoting: single quotes, double quotes
//! and backslash. A pipe, a redirection, a glob, a `$` and a `;` are ordinary
//! bytes of an argument.
//!
//! That is the refusal, not an omission: honouring `|` would need a shell, and
//! there is one — a client that wants those asks for `shell -c '…'`, whose
//! quoting this grammar hands to the shell intact.

/// Split an `exec` request line into a program and its arguments.
///
/// `Err` is the whole diagnostic: it names what is wrong with the line, and
/// the caller sends it to the client rather than running anything.
pub fn split(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    // A word exists as soon as a quote opens, so `''` is an empty argument and
    // not a missing one.
    let mut started = false;
    let mut chars = line.chars();

    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            '\'' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("unterminated ' quote".to_string()),
                    }
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        // Inside double quotes a backslash escapes only the
                        // two characters that could not otherwise be written;
                        // before anything else it is a backslash, which is
                        // what every shell does here.
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => return Err("a line may not end in a backslash".to_string()),
                        },
                        Some(c) => word.push(c),
                        None => return Err("unterminated \" quote".to_string()),
                    }
                }
            }
            '\\' => {
                started = true;
                match chars.next() {
                    Some(c) => word.push(c),
                    None => return Err("a line may not end in a backslash".to_string()),
                }
            }
            c => {
                started = true;
                word.push(c);
            }
        }
    }
    if started {
        words.push(word);
    }
    if words.is_empty() {
        return Err("no program named".to_string());
    }
    Ok(words)
}

/// Where a program name resolves to. A bare name is `/system/bin/<name>`,
/// which is what the shell's `PATH` holds and the only directory of programs
/// this system has; anything absolute is taken as written.
///
/// A name with a `/` in it that is *not* absolute is refused rather than
/// resolved against a working directory this daemon does not have.
pub fn resolve(name: &str) -> Result<String, String> {
    if name.starts_with('/') {
        return Ok(name.to_string());
    }
    if name.contains('/') {
        return Err(format!(
            "{name:?} is neither an absolute path nor a bare program name, and this daemon has \
             no working directory to resolve it against"
        ));
    }
    Ok(format!("/system/bin/{name}"))
}

/// The grammar, against lines a client actually sends. Host tests — `cargo test
/// --target "$(rustc -vV | sed -n 's/^host: //p')"` from this directory.
#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        split(line).expect("a line that splits")
    }

    #[test]
    fn a_bare_command_is_its_words() {
        assert_eq!(words("echo hello world"), ["echo", "hello", "world"]);
        assert_eq!(words("  echo   hello  "), ["echo", "hello"]);
        assert_eq!(words("echo"), ["echo"]);
    }

    #[test]
    fn quotes_hold_a_word_together() {
        assert_eq!(words("echo 'hello world'"), ["echo", "hello world"]);
        assert_eq!(words("echo \"hello world\""), ["echo", "hello world"]);
        assert_eq!(words("echo a' 'b"), ["echo", "a b"]);
        assert_eq!(words("echo ''"), ["echo", ""]);
    }

    /// The one construct that has to survive intact, because it is how a client
    /// reaches the shell this daemon refuses to be.
    #[test]
    fn a_shell_command_reaches_the_shell_whole() {
        assert_eq!(
            words("shell -c 'ls /system/bin | grep sshd'"),
            ["shell", "-c", "ls /system/bin | grep sshd"]
        );
    }

    /// A shell metacharacter is a byte of an argument and nothing else. The
    /// refusal is that it is *not* interpreted, so it must arrive whole.
    #[test]
    fn metacharacters_are_ordinary_bytes() {
        assert_eq!(words("echo a|b"), ["echo", "a|b"]);
        assert_eq!(words("echo >out"), ["echo", ">out"]);
        assert_eq!(words("echo $HOME"), ["echo", "$HOME"]);
        assert_eq!(words("echo a;b"), ["echo", "a;b"]);
        assert_eq!(words("echo *"), ["echo", "*"]);
    }

    #[test]
    fn backslash_escapes_one_character() {
        assert_eq!(words("echo a\\ b"), ["echo", "a b"]);
        assert_eq!(words("echo \"a\\\"b\""), ["echo", "a\"b"]);
        // Inside double quotes, a backslash before anything else stays.
        assert_eq!(words("echo \"a\\nb\""), ["echo", "a\\nb"]);
    }

    #[test]
    fn a_line_that_names_nothing_is_refused() {
        for line in ["", "   ", "\t\n"] {
            assert_eq!(split(line), Err("no program named".to_string()), "{line:?}");
        }
    }

    #[test]
    fn an_unfinished_quote_is_refused() {
        assert_eq!(split("echo 'x"), Err("unterminated ' quote".to_string()));
        assert_eq!(split("echo \"x"), Err("unterminated \" quote".to_string()));
        assert!(split("echo x\\").is_err());
    }

    #[test]
    fn a_bare_name_resolves_under_system_bin() {
        assert_eq!(resolve("echo").as_deref(), Ok("/system/bin/echo"));
        assert_eq!(resolve("/system/bin/echo").as_deref(), Ok("/system/bin/echo"));
        assert!(resolve("../etc/passwd").is_err());
        assert!(resolve("bin/echo").is_err());
    }
}
