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
const DEVICE_STARTED: &str = "soundd: resumed";

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


/// Every judge above against logs crafted to pass it and logs crafted to fail
/// it, one way each: the judges read a stick only the T14 comes back with, so
/// this is where each is shown able to red at all.
pub fn judges_verdict() -> Result<(), String> {
    let judged = |what: &str, judge: fn(&Serial) -> Result<(), String>, log: &str, green: bool| {
        match (judge(&Serial::named(what, log)), green) {
            (Ok(()), true) | (Err(_), false) => Ok(()),
            (Ok(()), false) => Err(format!("{what} passed a log it has to refuse:\n{log}")),
            (Err(why), true) => Err(format!("{what} refused a log it has to pass: {why}\n{log}")),
        }
    };
    let stats = |underruns: u32, clients: u32, deferred: u32| {
        format!(
            "{{2.000 soundd}} soundd: wakes=9 completions=9 submitted=9 underruns={underruns} \
             drains=0 max_wake_lat_us=9 max_batch=1 clients={clients} deferred={deferred} \
             starve_max=0 worst_irq_late_us=0 worst_pickup_us=0 worst_empty=0 worst_batch=1 \
             late_wakes=0\n"
        )
    };
    let spawn = |job: &str| format!("[kernel 1.000 cpu0] spawn: /system/bin/{job} pid=7\n");
    // One stream through soundd: `playing` is the stats line while it plays,
    // and `ended` the flush once it has left.
    let session = |playing: String, how: &str, ended: String| {
        format!(
            "{{1.000 soundd}} soundd: client 0 connected (id=1)\n{{1.000 soundd}} soundd: \
             resumed\n{playing}{{3.000 soundd}} soundd: client 1 removed ({how})\n{ended}\
             {{3.000 soundd}} soundd: suspended\n"
        )
    };
    let next = spawn("test_rs_next");

    let configured = "{0.500 soundd} soundd: hda path configured in 3 ms\n";
    let tone = |ended: String| {
        format!("{configured}{}{}", spawn("test_rs_audio_tone"), session(stats(0, 1, 0), "closed", ended))
    };
    judged("the tone", tone_on_metal, &format!("{}{next}", tone(stats(0, 0, 0))), true)?;
    judged("a tone short of periods", tone_on_metal, &format!("{}{next}", tone(stats(2, 0, 0))), false)?;
    judged(
        "a tone off no hda path",
        tone_on_metal,
        &format!("{}{next}", tone(stats(0, 0, 0))).replace(configured, ""),
        false,
    )?;
    judged(
        "a tone on the null sink",
        tone_on_metal,
        &format!("{}{{0.600 soundd}} {NULL_SINK}\n{next}", tone(stats(0, 0, 0))),
        false,
    )?;

    let stall = |second: String, rest: &str| {
        format!(
            "{}{}{second}{rest}",
            spawn("test_rs_hda_client_stall"),
            session(stats(3, 1, 0), "closed", stats(0, 0, 0))
        )
    };
    let second = session(stats(1, 1, 0), "closed", stats(0, 0, 0));
    judged("the stalled client", client_stall_on_metal, &stall(second.clone(), &next), true)?;
    judged(
        "a stalled client whose second stream never resumed soundd",
        client_stall_on_metal,
        &stall(second.replace("soundd: resumed\n", "soundd: client 0 streaming\n"), &next),
        false,
    )?;
    judged(
        "a stalled client soundd filled no period for",
        client_stall_on_metal,
        &stall(second.clone(), &next).replace("underruns=3", "underruns=0").replace("underruns=1", "underruns=0"),
        false,
    )?;
    // The next job spawned ahead of soundd's last word on the second stream.
    let (playing, tail) = second.split_at(second.find("{3.000 soundd} soundd: client 1 removed").expect("staged"));
    judged(
        "a stalled client soundd deferred for after the next job began",
        client_stall_on_metal,
        &stall(format!("{playing}{next}{}", tail.replace("deferred=0", "deferred=1")), ""),
        false,
    )?;
    judged(
        "a stalled client over a repeated completion",
        client_stall_on_metal,
        &stall(second.clone(), &format!("{{2.500 soundd}} soundd: repeated completion for free buffer\n{next}")),
        false,
    )?;

    let departures = |second: &str| {
        format!(
            "{}{}{}{next}",
            spawn("test_rs_null_sink_client_exits"),
            session(stats(0, 1, 0), "closed", stats(0, 0, 0)),
            session(stats(0, 1, 0), second, stats(0, 0, 0))
        )
    };
    judged("two departures", departures_on_metal, &departures("signal pipe gone"), true)?;
    judged("a departure soundd did not establish", departures_on_metal, &departures("died"), false)?;
    judged(
        "a death soundd claimed beside a departure it established",
        departures_on_metal,
        &departures("closed").replacen(
            "{3.000 soundd} soundd: client 1 removed",
            "{3.000 soundd} soundd: client 1 died\n{3.000 soundd} soundd: client 1 removed",
            1,
        ),
        false,
    )?;
    judged(
        "a departure soundd never reported",
        departures_on_metal,
        &departures("closed").replacen("{3.000 soundd} soundd: client 1 removed (closed)\n", "", 1),
        false,
    )?;
    judged(
        "one departure of two",
        departures_on_metal,
        &format!(
            "{}{}{next}",
            spawn("test_rs_null_sink_client_exits"),
            session(stats(0, 1, 0), "closed", stats(0, 0, 0))
        ),
        false,
    )?;

    let idle = |said: &str| format!("{}{said}{next}", spawn("test_rs_audio_idle_suspend"));
    judged("an idle soundd", idle_suspend_on_metal, &idle("{1.000 soundd} soundd: suspended\n"), true)?;
    judged(
        "an idle soundd that started the device",
        idle_suspend_on_metal,
        &idle(&format!("{{1.000 soundd}} {DEVICE_STARTED}\n")),
        false,
    )?;

    // Eleven lines owed after the boot and the two others: six refusals and
    // both others in `/log`, and five counted unwritten.
    let stalled = |held: u32, unwritten: u32, refusals: usize| {
        format!(
            "{{1.000 test_rs_soundd_log_stall}} flooded soundd with 10 refusals\n\
             {{1.000 test_rs_soundd_log_stall}} soundd refused 1 probe\n\
             {}{{1.100 soundd}} soundd: protocol violation (msg 7)\n\
             {{1.200 soundd}} soundd: opening stream: 48000 Hz\n\
             {{2.000 logd}} logd: reading soundd again, as `--stall-until` asked, with {held} of \
             its ring's 64 slots waiting\n\
             {{2.100 logd}} logd: {unwritten} record(s) of soundd's found its ring full\n",
            "{1.050 soundd} soundd: refusing connection, too many\n".repeat(refusals)
        )
    };
    judged("the stalled log", log_stall_on_metal, &stalled(64, 5, 6), true)?;
    judged("a log stall that never filled the ring", log_stall_on_metal, &stalled(40, 5, 6), false)?;
    judged("a log stall that lost a line silently", log_stall_on_metal, &stalled(64, 4, 6), false)?;
    judged("a log stall that counted nothing unwritten", log_stall_on_metal, &stalled(64, 0, 11), false)?;
    Ok(())
}
