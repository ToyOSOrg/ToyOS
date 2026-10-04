//! soundserver judged off the log the T14's stick came back with. Audio is judged on
//! metal and nowhere else: no QEMU guest test plays it.

use super::serial::Serial;

/// The text of `log` from the kernel's record of `job`'s spawn to the end of
/// what soundserver said about the `sessions` streams it played, one after another.
///
/// A session ends at soundserver's flush on its last client leaving — the stats line
/// that carries `clients=0`, which soundserver writes after the removal — and not at
/// the next job's spawn, which can land before it. A job that plays nothing
/// (`sessions` of 0) ends at the next test binary's spawn.
///
/// Sessions are refused unless each is one stream: soundserver counts one window
/// across every client it holds, so a session another job's stream shared has
/// no count that is the job's alone.
fn job_window<'a>(log: &'a str, job: &str, sessions: usize) -> Result<&'a str, String> {
    let head = format!("spawn: /system/bin/{job} ");
    let at = log.find(&head).ok_or_else(|| format!("no `{head}` record: {job} never ran"))?;
    let rest = &log[at + head.len()..];
    if sessions == 0 {
        return Ok(&rest[..rest.find("spawn: /system/bin/test_rs_").unwrap_or(rest.len())]);
    }
    let mut end = 0;
    for session in 1..=sessions {
        let flushed = rest[end..]
            .match_indices(SESSION_ENDED)
            .find(|&(line_at, _)| is_soundserver_stats(&rest[end..], line_at))
            .map(|(line_at, _)| end + line_at)
            .ok_or_else(|| {
                format!(
                    "soundserver never said session {session} of {job}'s {sessions} ended (a stats \
                     line with `{}`):\n{rest}",
                    SESSION_ENDED.trim()
                )
            })?;
        end = rest[flushed..].find('\n').map_or(rest.len(), |nl| flushed + nl + 1);
    }
    let window = &rest[..end];
    let said = |what: &str| {
        window.lines().filter(|l| l.contains("soundserver: client ") && l.contains(what)).count()
    };
    // A removal counts too: a stream soundserver held before the spawn connected
    // outside the window and leaves inside it.
    let (streams, removed) = (said(" connected (id="), said(" removed ("));
    if streams != sessions || removed != sessions {
        return Err(format!(
            "soundserver connected {streams} and removed {removed} stream(s) in {job}'s {sessions} \
             session(s): another job's stream shared soundserver with {job}'s, and no count in the \
             window is {job}'s alone:\n{window}"
        ));
    }
    Ok(window)
}

/// What soundserver's flush on its last client leaving carries, and no other stats
/// line does.
const SESSION_ENDED: &str = " clients=0 ";

/// Whether the match at `at` in `text` is on one of soundserver's stats lines.
fn is_soundserver_stats(text: &str, at: usize) -> bool {
    let line_start = text[..at].rfind('\n').map_or(0, |nl| nl + 1);
    text[line_start..at].contains("soundserver: wakes=")
}

/// The tone client played to its end through the HDA controller soundserver drives
/// itself, soundserver never fell back to the null sink, and no period of it went
/// unfilled.
pub fn tone_on_metal(log: &Serial) -> Result<(), String> {
    log.must_say("soundserver: hda path configured in")?;
    log.must_not_say(NULL_SINK)?;
    let window = job_window(log.text(), "test_rs_audio_tone", 1)?;
    let underruns = sum_field(window, "underruns");
    if underruns != 0 {
        return Err(format!(
            "soundserver filled {underruns} period(s) the tone client had not covered, on a client \
             that keeps its ring full:\n{window}"
        ));
    }
    Ok(())
}

/// soundserver's own word for the sink it took when the machine has none.
pub const NULL_SINK: &str = "soundserver: no audio device, presenting a null sink";

/// soundserver with no client costs no CPU, and the device is not what is running:
/// in a window no client connects in, soundserver never started the stream. A zero
/// CPU delta is the signature of a suspended soundserver and equally of one wedged
/// with the device running; the start line tells them apart.
pub fn idle_suspend_on_metal(log: &Serial) -> Result<(), String> {
    let window = job_window(log.text(), "test_rs_audio_idle_suspend", 0)?;
    if window.contains(DEVICE_STARTED) {
        return Err(format!(
            "`{DEVICE_STARTED}` with no client connected — soundserver's zero CPU is the device left \
             running, not a suspend:\n{window}"
        ));
    }
    Ok(())
}

/// What soundserver says as it starts the stream, before the first submit.
pub(crate) const DEVICE_STARTED: &str = "soundserver: resumed";

/// The T14's panic, staged on its own HDA ring: a client that stops producing
/// for longer than the DMA ring takes to come round. The engine replays every
/// buffer it completes, so soundserver has to fill the periods the client did not
/// cover (`underruns`) and may hold none of them back (`deferred`), across a
/// suspend and a resume.
///
/// The resume is the second stream's, which the job stages. Whether the first
/// stream finds soundserver suspended is the job before it: on a shared boot it can
/// open while soundserver still plays that job's tail out.
pub fn client_stall_on_metal(log: &Serial) -> Result<(), String> {
    const JOB: &str = "test_rs_hda_client_stall";
    log.must_not_say("repeated completion for free buffer")?;
    let first = job_window(log.text(), JOB, 1)?;
    let window = job_window(log.text(), JOB, 2)?;
    if !window[first.len()..].contains(DEVICE_STARTED) {
        return Err(format!(
            "no `{DEVICE_STARTED}` after the first stream's end — the second stream did not find \
             a suspended daemon, so nothing here tests a resume:\n{window}"
        ));
    }
    if !window.contains("soundserver: wakes=") {
        return Err(format!("soundserver reported no stats window while the client ran:\n{window}"));
    }
    if sum_field(window, "underruns") == 0 {
        return Err(format!(
            "soundserver filled no period the stalled client had not covered, so this boot staged \
             nothing:\n{window}"
        ));
    }
    let deferred = sum_field(window, "deferred");
    if deferred != 0 {
        return Err(format!(
            "soundserver deferred {deferred} period(s) on a ring that replays every one and completes \
             it again, which is the panic this exists for:\n{window}"
        ));
    }
    Ok(())
}

/// Two clients through soundserver, and what soundserver said about each leaving: every
/// removal names a departure soundserver established, and none claims a death.
pub fn departures_on_metal(log: &Serial) -> Result<(), String> {
    const CLIENTS: usize = 2;
    let window = job_window(log.text(), "test_rs_null_sink_client_exits", CLIENTS)?;
    let problems = check_departures(window, CLIENTS);
    if problems.is_empty() {
        return Ok(());
    }
    Err(format!("{}\n{window}", problems.join("\n")))
}

/// Sum one `soundserver:` counter across every stats window.
fn sum_field(serial: &str, key: &str) -> u32 {
    let needle = format!(" {key}=");
    serial
        .match_indices(&needle)
        .filter_map(|(at, _)| {
            let rest = &serial[at + needle.len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse::<u32>().ok()
        })
        .sum()
}

/// Every way a client left, as soundserver reported it: one entry per
/// `soundserver: client {id} removed ({how})` in `serial`.
///
/// The reason is read from the same line — a removal that names none yields the
/// empty string, which is what [`check_departures`] reds on.
fn departures(serial: &str) -> Vec<String> {
    serial
        .lines()
        .filter(|l| l.contains("soundserver: client ") && l.contains(" removed"))
        .map(|l| {
            let after = l.split(" removed").nth(1).unwrap_or("").trim();
            after
                .strip_prefix('(')
                .and_then(|r| r.split(')').next())
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

/// **soundserver may not report a departure it did not establish.**
///
/// A crash and a clean exit close the same descriptors in the same order, so
/// the mix loop's broken signal pipe witnesses neither — it used to say `died`
/// anyway, and did so on 5 of 44 runs whose client exited `code=0`.
/// Two things are asserted here, and the
/// second is the one with teeth: every removal names how the stream ended, in
/// the fixed departure vocabulary, and no line claims a death.
///
/// `expect` is how many removals the window must carry: a capture where no
/// client ever left would otherwise satisfy every check above it vacuously.
fn check_departures(serial: &str, expect: usize) -> Vec<String> {
    const KNOWN: [&str; 4] = ["closed", "refused", "disconnected", "signal pipe gone"];
    let mut problems = Vec::new();

    let seen = departures(serial);
    if seen.len() != expect {
        problems.push(format!(
            "soundserver reported {} client removals, expected {expect}",
            seen.len()
        ));
    }
    for how in &seen {
        if !KNOWN.contains(&how.as_str()) {
            problems.push(format!(
                "a client was removed with no departure soundserver established ({how:?}); \
                 the four soundserver establishes are {KNOWN:?}"
            ));
        }
    }
    // The word itself, whatever line carries it: soundserver cannot see a death and
    // must not print one.
    for line in serial.lines().filter(|l| l.contains("soundserver: ")) {
        if line.contains(" died") || line.contains(" crashed") {
            problems.push(format!(
                "soundserver claimed a client death it cannot distinguish from a clean exit: \
                 {line:?}"
            ));
        }
    }
    problems
}

/// soundserver's mix thread never waits on the log: `tests/logstallcase`'s `logkeeper`
/// reads nothing of soundserver's until the job says the tone has played, and the
/// job fills soundserver's ring with soundserver's own refusals before it plays. Judged
/// off `/log`:
///
/// 1. **The ring was full** — the premise: `logkeeper` found every one of its slots
///    waiting when the stall ended.
/// 2. **Nothing went unwritten silently**: every line soundserver's control thread
///    said after the boot is in `/log` or among the records `logkeeper` counted
///    unwritten, exactly, and some were counted — the flood is larger than the
///    ring.
pub fn log_stall_on_metal(log: &Serial) -> Result<(), String> {
    const REFUSAL: &str = "soundserver: refusing connection,";
    // `logkeeper`'s counts of soundserver's shared-ring records it could not write: its
    // lanes are the mix thread's, counted apart.
    const UNWRITTEN: [&str; 2] =
        [" record(s) of soundserver's found its ring full", " record(s) of soundserver's past its"];
    const RELEASED: &str = "logkeeper: reading soundserver again, as `--stall-until` asked, with ";
    const OF_SLOTS: &str = " of its ring's ";
    // The control thread's lines after the flood that are not refusals: the
    // probe it accepted, and the tone's stream. Either is in `/log` or counted.
    const AFTER_FLOOD: [&str; 2] = ["soundserver: protocol violation (msg ", "soundserver: opening stream: "];
    let number_before = |line: &str, marker: &str| -> Option<u64> {
        let at = line.find(marker)?;
        line[..at].rsplit(' ').next()?.parse().ok()
    };
    let number_after = |line: &str, marker: &str| -> Option<u64> {
        let rest = &line[line.find(marker)? + marker.len()..];
        rest.split(' ').next()?.parse().ok()
    };
    let text = log.text();
    let said_by_job = |marker: &str| -> Result<u64, String> {
        text.lines()
            .find_map(|l| number_after(l, marker))
            .ok_or_else(|| format!("the job never said {marker:?}:\n{text}"))
    };
    // Every refusal the job heard or provoked is one line soundserver said.
    let owed = said_by_job("flooded soundserver with ")? + said_by_job("soundserver refused ")?;

    let (mut refusals, mut unsaid, mut others, mut waiting) = (0u64, 0u64, 0u64, None);
    let soundserver = toyos_build::bootlog::lines_of(text, "soundserver");
    let logkeeper = toyos_build::bootlog::lines_of(text, "logkeeper");
    for line in soundserver.lines().chain(logkeeper.lines()) {
        if line.contains(REFUSAL) {
            refusals += 1;
        }
        unsaid += UNWRITTEN.iter().filter_map(|m| number_before(line, m)).sum::<u64>();
        if AFTER_FLOOD.iter().any(|m| line.contains(m)) {
            others += 1;
        }
        if let (Some(held), Some(slots)) = (number_after(line, RELEASED), number_after(line, OF_SLOTS)) {
            waiting = Some((held, slots));
        }
    }
    match waiting {
        Some((held, slots)) if held == slots && slots > 0 => {}
        Some((held, slots)) => {
            return Err(format!(
                "logkeeper found {held} of soundserver's {slots} ring slots waiting when its stall ended: \
                 the tone was not played to a full ring"
            ))
        }
        None => return Err("/log never says logkeeper's stall on soundserver ended".to_string()),
    }
    let said = owed + AFTER_FLOOD.len() as u64;
    if refusals + others + unsaid != said || unsaid == 0 {
        return Err(format!(
            "soundserver's control thread said {said} lines after the boot ({owed} refusals and {} \
             others); /log holds {refusals} refusals and {others} of the others, and logkeeper \
             counted {unsaid} unwritten",
            AFTER_FLOOD.len(),
        ));
    }
    Ok(())
}
