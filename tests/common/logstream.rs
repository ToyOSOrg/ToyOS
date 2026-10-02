//! The log a machine serves, judged from the host: a guest whose `logd` port
//! is forwarded, a reader that connects when the test says so, and the guest's
//! own `/log` as the oracle.
//!
//! **The file is what the stream is judged against.** `logd` writes each round
//! to `/log` and then hands the same bytes to every reader, from the boot's
//! first line however late the reader came, so what a reader received is the
//! file's own first lines in the file's own order ([`is_prefix_of`]). The file
//! is read off the FAT volume behind the guest's back, so the two readings
//! share nothing but the boot that produced them.

/// What a reader received is the file's own first lines, in the file's own
/// order, and nothing else.
///
/// Reported as the first disagreement rather than as a count: a stream that lost
/// its third line and one that reordered two are different defects, and a length
/// calls them the same one.
pub fn is_prefix_of(received: &[String], file: &[String]) -> Result<(), String> {
    if received.is_empty() {
        return Err("the reader received nothing at all".to_string());
    }
    for (i, line) in received.iter().enumerate() {
        match file.get(i) {
            Some(theirs) if theirs == line => {}
            Some(theirs) => {
                return Err(format!(
                    "the stream and /log disagree at line {i}:\n  stream: {line:?}\n  /log:   \
                     {theirs:?}"
                ))
            }
            None => {
                return Err(format!(
                    "the stream carries {} line(s) and /log only {}; the first line past the \
                     file is {line:?}",
                    received.len(),
                    file.len()
                ))
            }
        }
    }
    Ok(())
}
