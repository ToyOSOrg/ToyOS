//! The audio server: what it claims, what it spawns, and nothing else.
//!
//! Everything under `src/` is soundserver's own machinery:
//!
//! | | |
//! |---|---|
//! | [`backend`] | the two devices, and the one thing that differs between them |
//! | [`client`] | one stream: its ring, its ramp, and how it ended |
//! | [`command`] | the ring the control thread hands the mix thread |
//! | [`control`] | the connections, the framing, and what a client may ask for |
//! | [`inspect`] | what a reader of `inspect` is told, and the mix loop's side of it |
//! | [`mix`] | the two loops — a device's, and the null sink's |
//! | [`ring`] | the lock-free ring every hand-off to or from the mix thread is |
//! | [`log`] | soundserver's lines, and why the mix thread's never wait |
//! | [`output`] | the `sound-output` port: the output drivers, and the one played to |
//! | [`virtio`] | the virtio-sound driver, the one device this process drives itself |
//! | [`volume`] | the `volume` port: the master level and mute, on the codec |
//!
//! This file is which of them the machine gets. A virtio-sound function this
//! process was endowed with runs the device loop; anything else runs the null
//! sink until an output driver's offer is played to, because soundserver always
//! runs and always accepts streams.

use toyos::endow;
use toyos::port::Acceptor;
use toyos::wake::{self, Bell, Waker};
use toyos::PciDev;
use toyos_mixer::{period_frames, ramp_frames, DEVICE_RATES};
use toyos_sound_output as sound_output;

use std::sync::Arc;

/// One line of soundserver's, and never a wait: [`log`] is why.
macro_rules! say {
    ($($arg:tt)*) => {
        $crate::log::said(format_args!($($arg)*))
    };
}

mod backend;
mod client;
mod command;
mod control;
mod inspect;
mod mix;
mod log;
mod output;
mod ring;
mod virtio;
mod volume;

use backend::{Backend, VirtioBackend};
use command::CommandRing;
use inspect::{Device, Published, State};
use control::control_thread;
use mix::{mix_thread, null_sink_thread};
use output::{Attached, Outputs};

/// The virtual output soundserver presents until it plays to a device: the
/// first device rate, stereo, 128 frames per period. A stream negotiates
/// against them exactly as it would a real device, so a no-hardware machine is
/// invisible to the client.
pub(crate) const NULL_SINK: Device = Device {
    kind: "null",
    rate: DEVICE_RATES[0],
    channels: 2,
    period_frames: 128,
    buffers: NULL_SINK_BUFFERS as u32,
};
/// Same DMA-pipeline depth as both hardware backends: the client ring
/// is as deep, so a client may fill `NULL_SINK_BUFFERS - 1` periods ahead and
/// its backpressure is the device's. Power of two — ring indices wrap mod 2^32.
pub(crate) const NULL_SINK_BUFFERS: usize = 8;

fn main() {
    // First: `main` becomes the mix thread, and its lines are a lane's from here.
    log::claim_lane();
    let ports = Ports {
        acceptor: endow::acceptor("soundserver").expect("the manifest declares this program serves `soundserver`"),
        volume: endow::acceptor("volume").expect("the manifest declares this program serves `volume`"),
        output: endow::acceptor(sound_output::PORT)
            .expect("the manifest declares this program serves `sound-output`"),
    };

    // The virtio-sound function, where the machine has one, is this process's
    // own: no machine in this project has it beside an output driver's
    // function. The T14 and the AMD bench have output drivers' alone.
    if let Some(dev) = endow::claim::<PciDev>(&virtio::FUNCTION.to_string()) {
        match virtio::Virtio::claim(dev) {
            Ok((virtio, rate, channels)) => {
                let device = Device {
                    kind: "virtio-sound",
                    rate,
                    channels: channels as u16,
                    period_frames: 0,
                    buffers: toyos_virtio_sound::PERIODS as u32,
                };
                match period_frames(toyos_virtio_sound::PERIODS, device.channels, toyos_virtio_sound::PERIOD_BYTES, rate) {
                    Ok(frames) => {
                        let device = Device { period_frames: frames as u32, ..device };
                        return run(ports, Some((VirtioBackend { virtio }, device)));
                    }
                    Err(why) => say!("soundserver: this audio device's shape cannot carry audio: {why}"),
                }
            }
            Err(why) => say!("soundserver: the virtio-sound device cannot carry audio: {why}"),
        }
    }
    run(ports, None)
}

/// The three ports soundserver serves: streams, the master, and its outputs.
struct Ports {
    acceptor: Acceptor,
    volume: Acceptor,
    output: Acceptor,
}

/// Serve on `own`, the device this process drives, where there is one, and on
/// the null sink until an output is played to otherwise.
fn run(ports: Ports, own: Option<(VirtioBackend, Device)>) {
    let device = own.as_ref().map_or(NULL_SINK, |(_, device)| *device);
    let outputs = Outputs::new(ports.output, own.is_some());
    let cmd_ring = Arc::new(CommandRing::new());
    let (cmd_wake, cmd_bell) = wake::pair().expect("soundserver: no event for its commands");
    // The mix thread's word that it ended a device loop on its own finding.
    let (ended_wake, ended_bell) = wake::pair().expect("soundserver: no event for a device loop's end");
    let initial = if own.is_some() { State::Suspended } else { State::Idle };
    let published = Arc::new(Published::new(initial));

    let cmd_ring2 = cmd_ring.clone();
    let published2 = published.clone();
    std::thread::Builder::new()
        .name("soundserver-ctrl".into())
        .spawn(move || {
            control_thread(
                ports.acceptor,
                ports.volume,
                outputs,
                &cmd_ring2,
                &cmd_wake,
                &ended_bell,
                device,
                &published2,
            );
        })
        .expect("soundserver: failed to spawn control thread");

    if let Some((mut backend, device)) = own {
        let period_bytes = toyos_virtio_sound::PERIOD_BYTES;
        let why = serve_device(&mut backend, &cmd_ring, &published, &cmd_bell, &device, period_bytes, 0, &ended_wake);
        panic!("soundserver: its own virtio-sound device loop ended ({why})");
    }
    let mut generation = 0;
    loop {
        say!(
            "soundserver: no output yet, presenting a null sink ({}Hz {}ch, {} frames/period, streams discarded)",
            device.rate, device.channels, device.period_frames
        );
        let attached = null_sink_thread(&cmd_ring, &published, &cmd_bell, &device, generation);
        let Attached { mut backend, offer, period_frames, generation: playing } = *attached;
        let output = output_device(&offer, period_frames);
        say!(
            "soundserver: playing to the output offered, {}Hz {}ch, {} frames/period",
            output.rate, output.channels, output.period_frames
        );
        let period_bytes = offer.period_bytes as usize;
        let why = serve_device(&mut backend, &cmd_ring, &published, &cmd_bell, &output, period_bytes, playing, &ended_wake);
        say!("soundserver: the output is gone ({why})");
        generation = playing + 1;
    }
}

/// An output driver's offer, as the mixer and the control thread see it.
pub(crate) fn output_device(offer: &sound_output::Offer, period_frames: usize) -> Device {
    Device {
        kind: "driver",
        rate: offer.rate,
        channels: offer.channels as u16,
        period_frames: period_frames as u32,
        buffers: offer.periods as u32,
    }
}

fn serve_device(
    backend: &mut dyn Backend,
    cmd_ring: &CommandRing,
    published: &Published,
    cmd_bell: &Bell,
    device: &Device,
    period_bytes: usize,
    generation: u32,
    ended_wake: &Waker,
) -> String {
    say!(
        "soundserver: ready, {} buffers, {}Hz {}ch, {} bytes/period, {} frames/period",
        device.buffers, device.rate, device.channels, period_bytes, device.period_frames
    );
    mix_thread(
        backend,
        cmd_ring,
        published,
        cmd_bell,
        device.buffers as usize,
        device.rate,
        device.channels,
        period_bytes,
        device.period_frames as usize,
        ramp_frames(device.rate),
        generation,
        ended_wake,
    )
}
