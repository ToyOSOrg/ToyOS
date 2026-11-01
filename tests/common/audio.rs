//! soundserver judged off the log a bench's boot left on its disk. Audio is judged on
//! metal and nowhere else: no QEMU guest test plays it.

use super::serial::Serial;

/// The text of `log` from the kernel's record of `job`'s spawn to the end of
/// what soundserver said about the one stream it played.
///
/// The session ends at soundserver's flush on its last client leaving — the stats
/// line that carries `clients=0`, which soundserver writes after the removal — and
/// not at the next job's spawn, which can land before it.
///
/// It is refused unless it is one stream: soundserver counts one window across
/// every client it holds, so a session another job's stream shared has no count
/// that is the job's alone.
fn job_window<'a>(log: &'a str, job: &str) -> Result<&'a str, String> {
    let head = format!("spawn: /system/bin/{job} ");
    let at = log.find(&head).ok_or_else(|| format!("no `{head}` record: {job} never ran"))?;
    let rest = &log[at + head.len()..];
    let flushed = rest
        .match_indices(SESSION_ENDED)
        .find(|&(line_at, _)| is_soundserver_stats(rest, line_at))
        .map(|(line_at, _)| line_at)
        .ok_or_else(|| {
            format!(
                "soundserver never said {job}'s session ended (a stats line with `{}`):\n{rest}",
                SESSION_ENDED.trim()
            )
        })?;
    let window = &rest[..rest[flushed..].find('\n').map_or(rest.len(), |nl| flushed + nl + 1)];
    let said = |what: &str| {
        window.lines().filter(|l| l.contains("soundserver: client ") && l.contains(what)).count()
    };
    // A removal counts too: a stream soundserver held before the spawn connected
    // outside the window and leaves inside it.
    let (streams, removed) = (said(" connected (id="), said(" removed ("));
    if streams != 1 || removed != 1 {
        return Err(format!(
            "soundserver connected {streams} and removed {removed} stream(s) in {job}'s session: \
             another job's stream shared soundserver with {job}'s, and no count in the window is \
             {job}'s alone:\n{window}"
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

/// soundserver played to an output driver's device before `job` ran, so the
/// job's stream did not play to the null sink.
fn on_an_output(log: &str, job: &str) -> Result<(), String> {
    let playing = log
        .find(PLAYING)
        .ok_or_else(|| format!("soundserver never said `{PLAYING}`: no output was offered, and it stayed the null sink"))?;
    let head = format!("spawn: /system/bin/{job} ");
    let spawned = log.find(&head).ok_or_else(|| format!("no `{head}` record: {job} never ran"))?;
    if playing > spawned {
        return Err(format!("soundserver played to an output only after {job} spawned, so its stream played to the null sink"));
    }
    Ok(())
}

/// The tone client played to its end through the HDA controller an `hda`
/// driver drives, played to before it ran, and no period of it went unfilled.
pub fn tone_on_metal(log: &Serial) -> Result<(), String> {
    tone_played(log, "test_rs_audio_tone")
}

/// `job`'s tone played to an output whole, every period its own.
pub fn tone_played(log: &Serial, job: &str) -> Result<(), String> {
    on_an_output(log.text(), job)?;
    let window = job_window(log.text(), job)?;
    let underruns = sum_field(window, "underruns");
    if underruns != 0 {
        return Err(format!(
            "soundserver filled {underruns} period(s) the tone client had not covered, on a client \
             that keeps its ring full:\n{window}"
        ));
    }
    Ok(())
}

/// The job that plays `hda_tone`'s tone while it writes the log disk.
pub const TONE_BESIDE_LOG_WRITE: &str = "test_rs_tone_beside_log_write";

/// That job's tone played to its end through the HDA controller with no
/// period left to soundserver, while at least a mebibyte of its lines reached
/// the log disk: counted in the file read back off it, not in what the job says.
pub fn tone_beside_a_log_write(log: &Serial) -> Result<(), String> {
    on_an_output(log.text(), TONE_BESIDE_LOG_WRITE)?;
    let window = job_window(log.text(), TONE_BESIDE_LOG_WRITE)?;
    let reached = super::metal::bytes_of_lines(window, "tone_beside_log_write: line ");
    if reached < 1 << 20 {
        return Err(format!("{reached} bytes of the job's lines reached the disk beside the tone, under a mebibyte"));
    }
    let wrote = format!("{} KiB of lines reached the disk beside the tone", reached >> 10);
    let underruns = sum_field(window, "underruns");
    if underruns != 0 {
        return Err(format!(
            "soundserver filled {underruns} period(s) the tone client had not covered, beside a stick \
             write ({}):\n{window}",
            wrote.trim()
        ));
    }
    eprintln!("  [audio] {}", wrote.trim());
    Ok(())
}

/// The resampled tone client played to its end through the HDA controller, at
/// a rate the device does not run, and no period of it went unfilled.
pub fn resampled_tone_on_metal(log: &Serial) -> Result<(), String> {
    on_an_output(log.text(), "test_rs_audio_tone_resampled")?;
    let device = rate_after(log.text(), "soundserver: ready, ")
        .ok_or("soundserver never said the rate it runs the device at")?;
    let window = job_window(log.text(), "test_rs_audio_tone_resampled")?;
    let client = rate_after(window, "soundserver: opening stream: ")
        .ok_or_else(|| format!("soundserver opened no stream in the job's window:\n{window}"))?;
    if client == device {
        return Err(format!(
            "the stream opened at the device's own rate, {device} Hz, so nothing was resampled:\n{window}"
        ));
    }
    let underruns = sum_field(window, "underruns");
    if underruns != 0 {
        return Err(format!(
            "soundserver filled {underruns} period(s) the {client} Hz tone had not covered on a \
             {device} Hz device:\n{window}"
        ));
    }
    Ok(())
}

/// The first `<digits>Hz` after `marker` in `text`.
fn rate_after(text: &str, marker: &str) -> Option<u32> {
    let at = text.find(marker)? + marker.len();
    let rest = &text[at..];
    let hz = rest.find("Hz")?;
    let digits = rest[..hz].rsplit(|c: char| !c.is_ascii_digit()).next()?;
    digits.parse().ok()
}

/// soundserver's word for beginning to play to an output driver's device.
pub const PLAYING: &str = "soundserver: playing to the output offered";

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
