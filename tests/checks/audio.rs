use super::*;
use audio::*;
use serial::Serial;

pub fn judges_verdict() -> Result<(), String> {
    let judged = |what: &str, judge: fn(&Serial) -> Result<(), String>, log: &str, green: bool| {
        match (judge(&Serial::named(what, log)), green) {
            (Ok(()), true) | (Err(_), false) => Ok(()),
            (Ok(()), false) => Err(format!("{what} passed a log it has to refuse:\n{log}")),
            (Err(why), true) => Err(format!("{what} refused a log it has to pass: {why}\n{log}")),
        }
    };
    let stats = |underruns: u32, clients: u32| {
        format!(
            "[ 2.000 soundserver] soundserver: wakes=9 completions=9 submitted=9 underruns={underruns} \
             drains=0 max_wake_lat_us=9 max_batch=1 clients={clients} deferred=0 \
             starve_max=0 worst_irq_late_us=0 worst_pickup_us=0 worst_empty=0 worst_batch=1 \
             late_wakes=0\n"
        )
    };
    let spawn = |job: &str| format!("[ 1.000 cpu0 kernel] spawn: /system/bin/{job} pid=7\n");
    // One stream through soundserver: `playing` is the stats line while it plays,
    // and `ended` the flush once it has left.
    let session = |playing: String, ended: String| {
        format!(
            "[ 1.000 soundserver] soundserver: client 0 connected (id=1)\n[ 1.000 soundserver] soundserver: \
             resumed\n{playing}[ 3.000 soundserver] soundserver: client 1 removed (closed)\n{ended}\
             [ 3.000 soundserver] soundserver: suspended\n"
        )
    };
    let next = spawn("test_rs_next");

    let configured = "[ 0.500 soundserver] soundserver: playing to the output offered, 44100Hz 2ch, 128 frames/period\n";
    let tone = |ended: String| {
        format!("{configured}{}{}", spawn("test_rs_audio_tone"), session(stats(0, 1), ended))
    };
    judged("the tone", tone_on_metal, &format!("{}{next}", tone(stats(0, 0))), true)?;
    judged("a tone short of periods", tone_on_metal, &format!("{}{next}", tone(stats(2, 0))), false)?;
    judged(
        "a tone with no output played to",
        tone_on_metal,
        &format!("{}{next}", tone(stats(0, 0))).replace(configured, ""),
        false,
    )?;
    judged(
        "a tone that played before soundserver played to the output",
        tone_on_metal,
        &format!("{}{configured}{next}", tone(stats(0, 0)).replace(configured, "")),
        false,
    )?;
    // A job that waits after its spawn: the output offered after the spawn and
    // before its stream connects is the one it played to; offered after the
    // stream connected, it played to the null sink.
    let waiting = |offered_between: bool| {
        let spawned = spawn("test_rs_audio_tone");
        match offered_between {
            true => format!("{spawned}{configured}{}{next}", session(stats(0, 1), stats(0, 0))),
            false => format!("{spawned}{}{configured}{next}", session(stats(0, 1), stats(0, 0))),
        }
    };
    judged("a tone whose output was offered after its spawn and before its stream", tone_on_metal, &waiting(true), true)?;
    judged("a tone whose stream connected before the output was offered", tone_on_metal, &waiting(false), false)?;
    // The next job's stream connected before soundserver let the tone's go, so the
    // tone's last window is both streams' whatever it counted.
    judged(
        "a tone another job's stream joined",
        tone_on_metal,
        &format!(
            "{configured}{}[ 1.000 soundserver] soundserver: client 0 connected (id=1)\n{}{next}\
             [ 3.000 soundserver] soundserver: client 1 connected (id=2)\n\
             [ 3.000 soundserver] soundserver: client 1 removed (closed)\n{}\
             [ 4.000 soundserver] soundserver: client 2 removed (closed)\n{}",
            spawn("test_rs_audio_tone"),
            stats(0, 1),
            stats(0, 1),
            stats(0, 0),
        ),
        false,
    )?;
    judged(
        "a tone whose window ended before its stream connected",
        tone_on_metal,
        &format!(
            "{configured}{}[ 0.900 soundserver] soundserver: client 0 removed (closed)\n{}{}{next}",
            spawn("test_rs_audio_tone"),
            stats(0, 0),
            session(stats(0, 1), stats(0, 0))
        ),
        false,
    )?;
    // Another job's stream, held since before the spawn, is still held when the
    // tone's connects: one connect in the window, and two removals.
    judged(
        "a tone beside a stream soundserver held at its spawn",
        tone_on_metal,
        &format!(
            "{configured}[ 0.900 soundserver] soundserver: client 0 connected (id=0)\n{}\
             [ 1.000 soundserver] soundserver: client 1 connected (id=1)\n{}\
             [ 2.500 soundserver] soundserver: client 0 removed (closed)\n\
             [ 3.000 soundserver] soundserver: client 1 removed (closed)\n{}{next}",
            spawn("test_rs_audio_tone"),
            stats(0, 2),
            stats(0, 0),
        ),
        false,
    )?;
    Ok(())
}
