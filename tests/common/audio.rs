//! soundd judged off the log the T14's stick came back with. Audio is judged on
//! metal and nowhere else: no QEMU guest test plays it.

use super::serial::Serial;

/// The text of `log` from the kernel's record of `job`'s spawn to the end of
/// what soundd said about the `sessions` streams it played, one after another.
///
/// A session ends at soundd's flush on its last client leaving — the stats line
/// that carries `clients=0`, which soundd writes after the removal — and not at
/// the next job's spawn, which can land before it. A job that plays nothing
/// (`sessions` of 0) ends at the next test binary's spawn.
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
            .find(|&(line_at, _)| is_soundd_stats(&rest[end..], line_at))
            .map(|(line_at, _)| end + line_at)
            .ok_or_else(|| {
                format!(
                    "soundd never said session {session} of {job}'s {sessions} ended (a stats \
                     line with `{}`):\n{rest}",
                    SESSION_ENDED.trim()
                )
            })?;
        end = rest[flushed..].find('\n').map_or(rest.len(), |nl| flushed + nl + 1);
    }
    Ok(&rest[..end])
}

/// What soundd's flush on its last client leaving carries, and no other stats
/// line does.
const SESSION_ENDED: &str = " clients=0 ";

/// Whether the match at `at` in `text` is on one of soundd's stats lines.
fn is_soundd_stats(text: &str, at: usize) -> bool {
    let line_start = text[..at].rfind('\n').map_or(0, |nl| nl + 1);
    text[line_start..at].contains("soundd: wakes=")
}

/// The tone client played to its end through the HDA controller soundd drives
/// itself, soundd never fell back to the null sink, and no period of it went
/// unfilled.
pub fn tone_on_metal(log: &Serial) -> Result<(), String> {
    log.must_say("soundd: hda path configured in")?;
    log.must_not_say(NULL_SINK)?;
    let window = job_window(log.text(), "test_rs_audio_tone", 1)?;
    let underruns = sum_field(window, "underruns");
    if underruns != 0 {
        return Err(format!(
            "soundd filled {underruns} period(s) the tone client had not covered, on a client \
             that keeps its ring full:\n{window}"
        ));
    }
    Ok(())
}

/// soundd's own word for the sink it took when the machine has none.
pub const NULL_SINK: &str = "soundd: no audio device, presenting a null sink";

/// soundd with no client costs no CPU, and the device is not what is running:
/// in a window no client connects in, soundd never started the stream. A zero
/// CPU delta is the signature of a suspended soundd and equally of one wedged
/// with the device running; the start line tells them apart.
pub fn idle_suspend_on_metal(log: &Serial) -> Result<(), String> {
    let window = job_window(log.text(), "test_rs_audio_idle_suspend", 0)?;
    if window.contains(DEVICE_STARTED) {
        return Err(format!(
            "`{DEVICE_STARTED}` with no client connected — soundd's zero CPU is the device left \
             running, not a suspend:\n{window}"
        ));
    }
    Ok(())
}

/// What soundd says as it starts the stream, before the first submit.
pub(crate) const DEVICE_STARTED: &str = "soundd: resumed";

/// The T14's panic, staged on its own HDA ring: a client that stops producing
/// for longer than the DMA ring takes to come round. The engine replays every
/// buffer it completes, so soundd has to fill the periods the client did not
/// cover (`underruns`) and may hold none of them back (`deferred`), across a
/// suspend and a resume.
pub fn client_stall_on_metal(log: &Serial) -> Result<(), String> {
    log.must_not_say("repeated completion for free buffer")?;
    let window = job_window(log.text(), "test_rs_hda_client_stall", 2)?;
    let resumes = window.matches("soundd: resumed").count();
    if resumes < 2 {
        return Err(format!(
            "soundd resumed {resumes} time(s) — the second stream did not find a suspended \
             daemon, so nothing here tests a resume:\n{window}"
        ));
    }
    if !window.contains("soundd: wakes=") {
        return Err(format!("soundd reported no stats window while the client ran:\n{window}"));
    }
    if sum_field(window, "underruns") == 0 {
        return Err(format!(
            "soundd filled no period the stalled client had not covered, so this boot staged \
             nothing:\n{window}"
        ));
    }
    let deferred = sum_field(window, "deferred");
    if deferred != 0 {
        return Err(format!(
            "soundd deferred {deferred} period(s) on a ring that replays every one and completes \
             it again, which is the panic this exists for:\n{window}"
        ));
    }
    Ok(())
}

/// Two clients through soundd, and what soundd said about each leaving: every
/// removal names a departure soundd established, and none claims a death.
pub fn departures_on_metal(log: &Serial) -> Result<(), String> {
    const CLIENTS: usize = 2;
    let window = job_window(log.text(), "test_rs_null_sink_client_exits", CLIENTS)?;
    let problems = check_departures(window, CLIENTS);
    if problems.is_empty() {
        return Ok(());
    }
    Err(format!("{}\n{window}", problems.join("\n")))
}

/// Sum one `soundd:` counter across every stats window.
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

/// Every way a client left, as soundd reported it: one entry per
/// `soundd: client {id} removed ({how})` in `serial`.
///
/// The reason is read from the same line — a removal that names none yields the
/// empty string, which is what [`check_departures`] reds on.
fn departures(serial: &str) -> Vec<String> {
    serial
        .lines()
        .filter(|l| l.contains("soundd: client ") && l.contains(" removed"))
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

/// **soundd may not report a departure it did not establish.**
///
/// A crash and a clean exit close the same descriptors in the same order, so
/// the mix loop's broken signal pipe witnesses neither — it used to say `died`
/// anyway, and did so on 5 of 44 runs whose client exited `code=0`
/// (`issues/audio/`, closed). Two things are asserted here, and the
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
            "soundd reported {} client removals, expected {expect}",
            seen.len()
        ));
    }
    for how in &seen {
        if !KNOWN.contains(&how.as_str()) {
            problems.push(format!(
                "a client was removed with no departure soundd established ({how:?}); \
                 the four soundd establishes are {KNOWN:?}"
            ));
        }
    }
    // The word itself, whatever line carries it: soundd cannot see a death and
    // must not print one.
    for line in serial.lines().filter(|l| l.contains("soundd: ")) {
        if line.contains(" died") || line.contains(" crashed") {
            problems.push(format!(
                "soundd claimed a client death it cannot distinguish from a clean exit: \
                 {line:?}"
            ));
        }
    }
    problems
}

/// soundd's mix thread never waits on the log: `tests/logstallcase`'s `logd`
/// reads nothing of soundd's until the job says the tone has played, and the
/// job fills soundd's ring with soundd's own refusals before it plays. Judged
/// off `/log`:
///
/// 1. **The ring was full** — the premise: `logd` found every one of its slots
///    waiting when the stall ended.
/// 2. **Nothing went unwritten silently**: every line soundd's control thread
///    said after the boot is in `/log` or among the records `logd` counted
///    unwritten, exactly, and some were counted — the flood is larger than the
///    ring.
pub fn log_stall_on_metal(log: &Serial) -> Result<(), String> {
    const REFUSAL: &str = "soundd: refusing connection,";
    // `logd`'s counts of soundd's shared-ring records it could not write: its
    // lanes are the mix thread's, counted apart.
    const UNWRITTEN: [&str; 2] =
        [" record(s) of soundd's found its ring full", " record(s) of soundd's past its"];
    const RELEASED: &str = "logd: reading soundd again, as `--stall-until` asked, with ";
    const OF_SLOTS: &str = " of its ring's ";
    // The control thread's lines after the flood that are not refusals: the
    // probe it accepted, and the tone's stream. Either is in `/log` or counted.
    const AFTER_FLOOD: [&str; 2] = ["soundd: protocol violation (msg ", "soundd: opening stream: "];
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
    // Every refusal the job heard or provoked is one line soundd said.
    let owed = said_by_job("flooded soundd with ")? + said_by_job("soundd refused ")?;

    let (mut refusals, mut unsaid, mut others, mut waiting) = (0u64, 0u64, 0u64, None);
    let soundd = toyos_build::bootlog::lines_of(text, "soundd");
    let logd = toyos_build::bootlog::lines_of(text, "logd");
    for line in soundd.lines().chain(logd.lines()) {
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
                "logd found {held} of soundd's {slots} ring slots waiting when its stall ended: \
                 the tone was not played to a full ring"
            ))
        }
        None => return Err("/log never says logd's stall on soundd ended".to_string()),
    }
    let said = owed + AFTER_FLOOD.len() as u64;
    if refusals + others + unsaid != said || unsaid == 0 {
        return Err(format!(
            "soundd's control thread said {said} lines after the boot ({owed} refusals and {} \
             others); /log holds {refusals} refusals and {others} of the others, and logd \
             counted {unsaid} unwritten",
            AFTER_FLOOD.len(),
        ));
    }
    Ok(())
}
