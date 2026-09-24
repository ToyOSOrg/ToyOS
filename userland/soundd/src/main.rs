//! The audio server: what it claims, what it spawns, and nothing else.
//!
//! **Every decision soundd makes about a sample lives in `toyos-mixer/`** —
//! the i16/f32 conversions, mono and stereo, the gain and its ramp, the sum on
//! the shared bus, the dither and the quantizer, the device shapes a period can
//! be rendered into, the delay-locked loop, and the counters the gate reads.
//! They are pure, they are host-tested, and `toyos-mixer/fixtures/mix-corpus.txt`
//! holds the answer this program used to compute inline, byte for byte. What is
//! left here is the effects: devices, handles, shared memory, timers, threads
//! and the log.
//!
//! The split is `userland/compositor/`'s, against `toyos-desktop/`. Everything
//! under `src/` is one half of soundd's own machinery:
//!
//! | | |
//! |---|---|
//! | [`backend`] | the two devices, and the one thing that differs between them |
//! | [`client`] | one stream: its ring, its ramp, and how it ended |
//! | [`command`] | the ring the control thread hands the mix thread |
//! | [`control`] | the connections, the framing, and what a client may ask for |
//! | [`mix`] | the two loops — a device's, and the null sink's |
//! | [`ring`] | the lock-free ring every hand-off to or from the mix thread is |
//! | [`say`] | the one thread that writes soundd's output, so no other waits on it |
//! | [`hda`], [`virtio`] | the two drivers |
//!
//! This file is the fourth thing: which of them the machine gets. A sound card
//! this process was endowed with runs the device loop; anything else — no card,
//! a card that cannot carry audio, a shape the mixer cannot render — runs the
//! null sink, because soundd always runs and always accepts streams.

use toyos::endow;
use toyos::port::Acceptor;
use toyos::shm::SharedMemory;
use toyos::{HdaDev, VirtioSoundDev};
use toyos_abi::syscall::{self, DeviceType};
use toyos_mixer::{period_frames, ramp_frames};

use std::sync::Arc;

/// One line, handed to the one thread that writes soundd's output, and never a
/// wait: [`say`] is why, and what happens to a line that finds no room.
macro_rules! say {
    ($($arg:tt)*) => {{
        let mut line = format!($($arg)*);
        line.push('\n');
        $crate::say::said(line);
    }};
}

mod backend;
mod client;
mod command;
mod control;
mod hda;
mod mix;
mod ring;
mod say;
mod virtio;

use backend::{Backend, HdaBackend, VirtioBackend};
use command::CommandRing;
use control::control_thread;
use mix::{mix_thread, null_sink_thread};

/// The virtual output soundd presents when the machine has no audio hardware.
/// These match the one configuration cpal's ToyOS backend advertises
/// (`src/host/toyos/mod.rs`): 44100 Hz stereo i16, 128 frames per period. A
/// stream negotiates against them exactly as it would a real device, so a
/// no-hardware machine is invisible to the client.
const NULL_SINK_RATE: u32 = 44_100;
const NULL_SINK_CHANNELS: u16 = 2;
const NULL_SINK_PERIOD_FRAMES: usize = 128;
/// Same DMA-pipeline depth as both hardware backends: the client ring
/// is as deep, so a client may fill `NULL_SINK_BUFFERS - 1` periods ahead and
/// its backpressure is the device's. Power of two — ring indices wrap mod 2^32.
pub(crate) const NULL_SINK_BUFFERS: usize = 8;

fn main() {
    say::start(say::Voice::Mix);
    let acceptor = endow::acceptor("soundd")
        .expect("the manifest declares this program serves `soundd`");

    // **"Which sound card does this machine have?" is already answered.** init
    // mints a claim per class the manifest names and endows what the machine
    // actually had, so an absent card is a label missing from this process's
    // own table rather than two probing syscalls — and a card another process
    // holds is not a state that can arise, because only init mints.
    //
    // A machine with no sound card is a routing state and not a bug: soundd
    // presents a virtual output and discards what is played to it, so a client
    // building a stream succeeds whether or not hardware is present.
    //
    // The order is virtio first, and it is not a preference between two cards:
    // no machine in this project has both. The T14 has only the second.
    if let Some(dev) = endow::device::<VirtioSoundDev>(DeviceType::VirtioSound) {
        match virtio::Virtio::claim(dev) {
            Ok((virtio, rate, channels)) => return run_virtio(acceptor, virtio, rate, channels),
            Err(why) => {
                say!("soundd: the virtio-sound device cannot carry audio: {why}");
                return run_null_sink(acceptor);
            }
        }
    }
    let Some(dev) = endow::device::<HdaDev>(DeviceType::HdaAudio) else {
        return run_null_sink(acceptor);
    };
    match hda::Hda::claim(dev) {
        Ok((hda, _path, channels)) => run_hda(acceptor, hda, channels),
        Err(why) => {
            say!("soundd: the HDA controller cannot carry audio: {why}");
            run_null_sink(acceptor)
        }
    }
}

fn run_virtio(acceptor: Acceptor, virtio: virtio::Virtio, rate: u32, channels: u8) {
    run_with_device(
        acceptor,
        &mut VirtioBackend { virtio },
        toyos_abi::virtio_sound::PERIODS,
        rate,
        channels as u16,
        toyos_abi::virtio_sound::PERIOD_BYTES,
    );
}

fn run_hda(acceptor: Acceptor, hda: hda::Hda, channels: u8) {
    let info = hda.info();
    let num_buffers = info.periods as usize;
    let period_bytes = info.period_bytes as usize;
    let ring = SharedMemory::adopt(info.pcm, 2 * 1024 * 1024)
        .expect("the PCM buffer the HDA claim just handed over");
    // One region, `periods` buffers end to end: the buffer descriptor list the
    // kernel built points at exactly these offsets.
    let base = ring.as_ptr();
    let buffers = (0..num_buffers).map(|i| unsafe { base.add(i * period_bytes) }).collect();

    run_with_device(
        acceptor,
        &mut HdaBackend { hda, buffers, period_bytes },
        num_buffers,
        toyos_hda::config::RATE,
        channels as u16,
        period_bytes,
    );
}

fn run_with_device(
    acceptor: Acceptor,
    backend: &mut dyn Backend,
    num_buffers: usize,
    device_sample_rate: u32,
    device_channels: u16,
    device_period_bytes: usize,
) {
    // A shape this mixer cannot render is named and the machine gets the null
    // sink, which keeps soundd always running and accepting streams. It is checked before any arithmetic
    // derives anything from it — a zero channel count divides by zero on the
    // way to a frame count, which is a panic that names neither the device nor
    // the reason.
    let device_period_frames = match period_frames(num_buffers, device_channels, device_period_bytes, device_sample_rate) {
        Ok(frames) => frames,
        Err(why) => {
            say!("soundd: this audio device's shape cannot carry audio: {why}");
            return run_null_sink(acceptor);
        }
    };

    // Client ring depth matches the DMA pipeline depth: a wake gap can free
    // at most num_buffers periods, so a full client ring always covers it.
    let slot_count = num_buffers as u32;

    let ramp_frames = ramp_frames(device_sample_rate);

    say!("soundd: ready, {} buffers, {}Hz {}ch, {} bytes/period, {} frames/period",
        num_buffers, device_sample_rate, device_channels, device_period_bytes, device_period_frames);

    let cmd_ring = Arc::new(CommandRing::new());
    let cmd_pipe = syscall::pipe().expect("soundd: failed to create the command pipe");

    let cmd_ring2 = cmd_ring.clone();
    std::thread::Builder::new()
        .name("soundd-ctrl".into())
        .spawn(move || {
            say::speak_as(say::Voice::Control);
            control_thread(
                acceptor,
                &cmd_ring2,
                cmd_pipe.write,
                device_sample_rate,
                device_channels,
                device_period_frames as u32,
                slot_count,
                ramp_frames,
            );
        })
        .expect("soundd: failed to spawn control thread");

    mix_thread(
        backend,
        &cmd_ring,
        cmd_pipe.read,
        num_buffers,
        device_sample_rate,
        device_channels,
        device_period_bytes,
        device_period_frames,
        ramp_frames,
    );
}

fn run_null_sink(acceptor: Acceptor) {
    let device_sample_rate = NULL_SINK_RATE;
    let device_channels = NULL_SINK_CHANNELS;
    let device_period_frames = NULL_SINK_PERIOD_FRAMES;
    let slot_count = NULL_SINK_BUFFERS as u32;

    let ramp_frames = ramp_frames(device_sample_rate);

    say!(
        "soundd: no audio device, presenting a null sink ({}Hz {}ch, {} frames/period, streams discarded)",
        device_sample_rate, device_channels, device_period_frames
    );

    let cmd_ring = Arc::new(CommandRing::new());
    let cmd_pipe = syscall::pipe().expect("soundd: failed to create the command pipe");

    let cmd_ring2 = cmd_ring.clone();
    std::thread::Builder::new()
        .name("soundd-ctrl".into())
        .spawn(move || {
            say::speak_as(say::Voice::Control);
            control_thread(
                acceptor,
                &cmd_ring2,
                cmd_pipe.write,
                device_sample_rate,
                device_channels,
                device_period_frames as u32,
                slot_count,
                ramp_frames,
            );
        })
        .expect("soundd: failed to spawn control thread");

    null_sink_thread(
        &cmd_ring,
        cmd_pipe.read,
        device_sample_rate,
        device_channels,
        device_period_frames,
        ramp_frames,
    );
}
