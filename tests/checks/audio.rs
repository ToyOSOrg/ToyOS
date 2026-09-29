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
