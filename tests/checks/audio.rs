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
            "[ 2.000 soundserver] soundserver: wakes=9 completions=9 submitted=9 underruns={underruns} \
             drains=0 max_wake_lat_us=9 max_batch=1 clients={clients} deferred={deferred} \
             starve_max=0 worst_irq_late_us=0 worst_pickup_us=0 worst_empty=0 worst_batch=1 \
             late_wakes=0\n"
        )
    };
    let spawn = |job: &str| format!("[ 1.000 cpu0 kernel] spawn: /system/bin/{job} pid=7\n");
    // One stream through soundserver: `playing` is the stats line while it plays,
    // and `ended` the flush once it has left.
    let session = |playing: String, how: &str, ended: String| {
        format!(
            "[ 1.000 soundserver] soundserver: client 0 connected (id=1)\n[ 1.000 soundserver] soundserver: \
             resumed\n{playing}[ 3.000 soundserver] soundserver: client 1 removed ({how})\n{ended}\
             [ 3.000 soundserver] soundserver: suspended\n"
        )
    };
    let next = spawn("test_rs_next");

    let configured = "[ 0.500 soundserver] soundserver: hda path configured in 3 ms\n";
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
        &format!("{}[ 0.600 soundserver] {NULL_SINK}\n{next}", tone(stats(0, 0, 0))),
        false,
    )?;
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
            stats(0, 1, 0),
            stats(0, 1, 0),
            stats(0, 0, 0),
        ),
        false,
    )?;
    judged(
        "a tone whose window ended before its stream connected",
        tone_on_metal,
        &format!(
            "{configured}{}[ 0.900 soundserver] soundserver: client 0 removed (closed)\n{}{}{next}",
            spawn("test_rs_audio_tone"),
            stats(0, 0, 0),
            session(stats(0, 1, 0), "closed", stats(0, 0, 0))
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
            stats(0, 2, 0),
            stats(0, 0, 0),
        ),
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
        "a stalled client whose second stream never resumed soundserver",
        client_stall_on_metal,
        &stall(second.replace("soundserver: resumed\n", "soundserver: client 0 streaming\n"), &next),
        false,
    )?;
    judged(
        "a stalled client soundserver filled no period for",
        client_stall_on_metal,
        &stall(second.clone(), &next).replace("underruns=3", "underruns=0").replace("underruns=1", "underruns=0"),
        false,
    )?;
    // The next job spawned ahead of soundserver's last word on the second stream.
    let (playing, tail) = second.split_at(second.find("[ 3.000 soundserver] soundserver: client 1 removed").expect("staged"));
    judged(
        "a stalled client soundserver deferred for after the next job began",
        client_stall_on_metal,
        &stall(format!("{playing}{next}{}", tail.replace("deferred=0", "deferred=1")), ""),
        false,
    )?;
    judged(
        "a stalled client over a repeated completion",
        client_stall_on_metal,
        &stall(second.clone(), &format!("[ 2.500 soundserver] soundserver: repeated completion for free buffer\n{next}")),
        false,
    )?;
    judged("the T14's stalled client", client_stall_on_metal, T14_STALL, true)?;
    judged(
        "the T14's stalled client, its second stream opened on a running soundserver",
        client_stall_on_metal,
        &T14_STALL
            .replace("[2026-10-03 07:05:06  7.865 soundserver] soundserver: suspended\n", "")
            .replace("[2026-10-03 07:05:07  8.143 soundserver] soundserver: resumed\n", ""),
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
    judged("a departure soundserver did not establish", departures_on_metal, &departures("died"), false)?;
    judged(
        "a death soundserver claimed beside a departure it established",
        departures_on_metal,
        &departures("closed").replacen(
            "[ 3.000 soundserver] soundserver: client 1 removed",
            "[ 3.000 soundserver] soundserver: client 1 died\n[ 3.000 soundserver] soundserver: client 1 removed",
            1,
        ),
        false,
    )?;
    judged(
        "a departure soundserver never reported",
        departures_on_metal,
        &departures("closed").replacen("[ 3.000 soundserver] soundserver: client 1 removed (closed)\n", "", 1),
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
    judged("an idle soundserver", idle_suspend_on_metal, &idle("[ 1.000 soundserver] soundserver: suspended\n"), true)?;
    judged(
        "an idle soundserver that started the device",
        idle_suspend_on_metal,
        &idle(&format!("[ 1.000 soundserver] {DEVICE_STARTED}\n")),
        false,
    )?;

    // Eleven lines owed after the boot and the two others: six refusals and
    // both others in `/log`, and five counted unwritten.
    let stalled = |held: u32, unwritten: u32, refusals: usize| {
        format!(
            "[ 1.000 test_rs_soundserver_log_stall] flooded soundserver with 10 refusals\n\
             [ 1.000 test_rs_soundserver_log_stall] soundserver refused 1 probe\n\
             {}[ 1.100 soundserver] soundserver: protocol violation (msg 7)\n\
             [ 1.200 soundserver] soundserver: opening stream: 48000 Hz\n\
             [ 2.000 logkeeper] logkeeper: reading soundserver again, as `--stall-until` asked, with {held} of \
             its ring's 64 slots waiting\n\
             [ 2.100 logkeeper] logkeeper: {unwritten} record(s) of soundserver's found its ring full\n",
            "[ 1.050 soundserver] soundserver: refusing connection, too many\n".repeat(refusals)
        )
    };
    judged("the stalled log", log_stall_on_metal, &stalled(64, 5, 6), true)?;
    judged("a log stall that never filled the ring", log_stall_on_metal, &stalled(40, 5, 6), false)?;
    judged("a log stall that lost a line silently", log_stall_on_metal, &stalled(64, 4, 6), false)?;
    judged("a log stall that counted nothing unwritten", log_stall_on_metal, &stalled(64, 0, 11), false)?;
    Ok(())
}

/// The T14's `testcases` boot, verbatim: the tone's last records, and the
/// stalled client's from its spawn to its exit. Its first stream opens 3 ms
/// after the tone's left, on a soundserver that has not suspended.
const T14_STALL: &str = r"[2026-10-03 07:05:04  5.681 soundserver] soundserver: client 0 removed (closed)
[2026-10-03 07:05:04  5.681 soundserver] soundserver: wakes=573 completions=415 submitted=415 underruns=0 drains=0 max_wake_lat_us=2307 max_batch=2 clients=0 deferred=0 starve_max=0 worst_irq_late_us=2288 worst_pickup_us=18 worst_empty=0 worst_batch=2 late_wakes=0
[2026-10-03 07:05:04  5.682 cpu4 kernel] exit: test_rs_audio_tone pid=11 code=0 cpu=2ms
[2026-10-03 07:05:04  5.683 cpu7 kernel] spawn: /system/bin/test_rs_hda_client_stall pid=12 tid=0 dst=6 base=0x10000000000 entry=0x1000003df40 root=0x83d6000 symbols=2048KiB (layout=0ms relocs=0ms deps=0ms tls=0ms total=1ms)
[2026-10-03 07:05:04  5.684 soundserver tid=1] soundserver: opening stream: 44100Hz 2ch fmt=0
[2026-10-03 07:05:04  5.684 soundserver] soundserver: client 0 connected (id=1)
[2026-10-03 07:05:06  7.685 soundserver] soundserver: wakes=842 completions=690 submitted=690 underruns=104 drains=0 max_wake_lat_us=1746 max_batch=2 clients=1 deferred=0 starve_max=13 worst_irq_late_us=1724 worst_pickup_us=22 worst_empty=0 worst_batch=2 late_wakes=0
[2026-10-03 07:05:06  7.842 soundserver] soundserver: client 1 removed (closed)
[2026-10-03 07:05:06  7.842 soundserver] soundserver: wakes=85 completions=54 submitted=54 underruns=0 drains=0 max_wake_lat_us=70 max_batch=1 clients=0 deferred=0 starve_max=0 worst_irq_late_us=17 worst_pickup_us=53 worst_empty=1 worst_batch=1 late_wakes=0
[2026-10-03 07:05:06  7.842 cpu7 kernel tid=1] exit: test_rs_hda_client_stall tid=1 code=0 cpu=15ms
[2026-10-03 07:05:06  7.865 soundserver] soundserver: suspended
[2026-10-03 07:05:07  8.142 soundserver tid=1] soundserver: opening stream: 44100Hz 2ch fmt=0
[2026-10-03 07:05:07  8.142 soundserver] soundserver: client 0 connected (id=2)
[2026-10-03 07:05:07  8.143 soundserver] soundserver: resumed
[2026-10-03 07:05:08  9.051 soundserver] soundserver: client 2 removed (closed)
[2026-10-03 07:05:08  9.051 soundserver] soundserver: wakes=425 completions=313 submitted=321 underruns=26 drains=0 max_wake_lat_us=88 max_batch=1 clients=0 deferred=0 starve_max=13 worst_irq_late_us=53 worst_pickup_us=34 worst_empty=1 worst_batch=1 late_wakes=0
[2026-10-03 07:05:08  9.051 cpu0 kernel tid=2] exit: test_rs_hda_client_stall tid=2 code=0 cpu=3ms
[2026-10-03 07:05:08  9.079 soundserver] soundserver: suspended
[2026-10-03 07:05:08  9.079 soundserver] soundserver: idle wake 1 (1 records)
[2026-10-03 07:05:08  9.351 test-runner pid=12] stalled 8 then 2 times, soundserver survived
[2026-10-03 07:05:08  9.352 cpu6 kernel] exit: test_rs_hda_client_stall pid=12 code=0 cpu=2ms
";
