//! The verbs that turn a chosen path into a path that makes sound.
//!
//! The whole configuration step as one pure function, in the order it has to be
//! sent: power, the connection selects, pin control, EAPD, the amplifiers, the
//! format and the stream tag. The driver sends what comes back and decides
//! nothing on the way.
//!
//! Boundary contract: every payload layout below is the Intel High Definition
//! Audio specification's. They are here rather than in the driver because a
//! wrong bit is a path that configures perfectly and plays silence, and here it
//! is a host test against the graph of the machine that has to work.

use alloc::vec::Vec;

use crate::caps::{AmpCaps, GainRange, PcmCaps};
use crate::graph::{Codec, FunctionGroup};
use crate::path::{OutputPath, PinSetup};
use crate::verb::{self, Address, Node, Response, Verb};

/// `Set Power State`'s fully-on value.
const POWER_D0: u8 = 0x00;

/// `Set Pin Widget Control`: drive the pin as an output, and drive it hard
/// enough for headphones.
const PIN_OUT_ENABLE: u8 = 1 << 6;
const PIN_HEADPHONE_ENABLE: u8 = 1 << 7;

/// `Set EAPD/BTL Enable`: the external amplifier bit, and nothing else. A
/// speaker pin that has this and is not told to use it is a correctly
/// configured path that makes no sound.
const EAPD_ENABLE: u8 = 1 << 1;

/// `Set Amplifier Gain/Mute`: which amplifier and which channels the payload
/// applies to. Output, left and right — this driver sets both channels to one
/// value, so there is no per-channel index to carry.
const AMP_SET_OUTPUT: u16 = 1 << 15;
const AMP_SET_LEFT: u16 = 1 << 13;
const AMP_SET_RIGHT: u16 = 1 << 12;
const AMP_SET_MUTE: u16 = 1 << 7;

/// The whole configuration sequence for one output path, in the order it has to
/// be sent.
///
/// `None` when the codec or the function group the path names is not in
/// `codecs` — which cannot happen for a path this crate produced, and is a
/// refusal rather than a panic because the caller is holding untrusted device
/// answers either way.
pub fn verbs(
    codecs: &[Codec],
    path: &OutputPath,
    format: u16,
    stream_tag: u8,
) -> Option<Vec<Verb>> {
    let group = codecs
        .iter()
        .find(|c| c.address == path.codec)?
        .groups
        .iter()
        .find(|g| g.node == path.group)?;
    let codec = path.codec;
    let mut out = Vec::new();

    // Power first: a widget still in D3 answers every later verb and acts on
    // none of them.
    out.push(power(codec, group.node));
    for node in nodes(path) {
        if group.widget(node).is_some_and(|w| w.caps.power_control) {
            out.push(power(codec, node));
        }
    }

    // Then the route, so the converter is reaching the pin before the pin is
    // told to drive.
    for pin in pins(path) {
        for hop in &pin.route {
            out.push(Verb::short(codec, hop.node, verb::SET_CONNECTION_SELECT, hop.select));
        }
    }

    for pin in pins(path) {
        let mut control = PIN_OUT_ENABLE;
        if pin.headphone_drive {
            control |= PIN_HEADPHONE_ENABLE;
        }
        out.push(Verb::short(codec, pin.node, verb::SET_PIN_CONTROL, control));
        if pin.eapd {
            out.push(Verb::short(codec, pin.node, verb::SET_EAPD, EAPD_ENABLE));
        }
        if let Some(amp) = pin.amp {
            out.push(amp_verb(codec, pin.node, amp));
        }
    }

    if let Some(amp) = path.converter_amp {
        out.push(amp_verb(codec, path.converter, amp));
    }
    out.push(Verb::long(codec, path.converter, verb::SET_CONVERTER_FORMAT as u8, format));
    // Channel 0 of the stream the controller was given. The tag is the
    // controller's and this verb is the only place the codec learns it.
    out.push(Verb::short(codec, path.converter, verb::SET_CONVERTER_STREAM, stream_tag << 4));
    Some(out)
}

/// Unmuted, and at the amplifier's own 0 dB index where it has one.
///
/// The two halves are independent because the codec makes them independent:
/// the laptop's pin amplifier is mute-only with no gain field to write, and its
/// converter has 88 steps and no mute bit. Writing the absent half is a store
/// that succeeds and does nothing, which is worse than not writing it.
fn amp_verb(codec: Address, node: Node, amp: AmpCaps) -> Verb {
    let gain = amp.gain.map_or(0, |range| range.zero_db as u16);
    Verb::long(
        codec,
        node,
        verb::SET_AMP_GAIN_MUTE as u8,
        AMP_SET_OUTPUT | AMP_SET_LEFT | AMP_SET_RIGHT | gain,
    )
}

fn power(codec: Address, node: Node) -> Verb {
    Verb::short(codec, node, verb::SET_POWER_STATE, POWER_D0)
}

fn pins(path: &OutputPath) -> impl Iterator<Item = &PinSetup> {
    core::iter::once(&path.output).chain(path.headphone.iter())
}

/// Every node on the path, converter included, without repeats: a hop shared by
/// the speaker and the jack would otherwise be powered twice.
fn nodes(path: &OutputPath) -> Vec<Node> {
    let mut out = alloc::vec![path.converter];
    for pin in pins(path) {
        for hop in &pin.route {
            if !out.contains(&hop.node) {
                out.push(hop.node);
            }
        }
        if !out.contains(&pin.node) {
            out.push(pin.node);
        }
    }
    out
}

/// The width this driver asks a converter for.
pub const WIDTH: u8 = 16;

/// The first of `rates`, best first, that the path's converter offers at
/// [`WIDTH`]; or, where it offers none of them, the converter's own PCM word
/// (`None` where it has none), so a refusal can name what it does offer.
///
/// The converter's own word, never its function group's: every converter in
/// reach sets Format Override, and one that did not would take the group's.
pub fn rate(codecs: &[Codec], path: &OutputPath, rates: &[u32]) -> Result<u32, Option<PcmCaps>> {
    let pcm = group_of(codecs, path)
        .and_then(|group| group.widget(path.converter))
        .and_then(|converter| converter.pcm)
        .ok_or(None)?;
    rates.iter().copied().find(|&rate| pcm.supports(rate, WIDTH)).ok_or(Some(pcm))
}

/// `SDnFMT` and the codec's `Set Converter Format` payload for this path at
/// `rate`, or `None` where the converter does not offer `rate` at [`WIDTH`].
pub fn format(codecs: &[Codec], path: &OutputPath, rate: u32) -> Option<(u16, u8)> {
    let group = group_of(codecs, path)?;
    let converter = group.widget(path.converter)?;
    let pcm = converter.pcm?;
    if !pcm.supports(rate, WIDTH) {
        return None;
    }
    let channels = converter.caps.channels.min(2);
    Some((crate::stream::stream_format(rate, WIDTH, channels)?, channels))
}

/// One position of the master level, in quarter-decibels: 3 dB, quantised up
/// to whole steps of the amplifier that carries it.
const POSITION_QUARTER_DB: u8 = 12;

/// `Get Amplifier Gain/Mute`'s payload: the output amplifier, left channel.
const AMP_GET_OUTPUT_LEFT: u16 = (1 << 15) | (1 << 13);

/// A `Get Amplifier Gain/Mute` answer's mute bit, and its gain index.
const AMP_ANSWER_MUTE: u32 = 1 << 7;
const AMP_ANSWER_GAIN: u32 = 0x7F;

/// The path's master level and mute, on the codec.
///
/// The level is on the path's amplifier that has a gain range, and moves in
/// positions of 3 dB from its 0 dB, position 0, down to its minimum. Mute is
/// on every output pin amplifier with a mute bit, or else on the converter's.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Master {
    codec: Address,
    gain: Node,
    range: GainRange,
    mute: Vec<Node>,
}

/// Where the master stands: a position below the top, and whether it is muted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Setting {
    pub position: u8,
    pub muted: bool,
}

impl Setting {
    /// 0 dB and unmuted: what [`verbs`] leaves a path at.
    pub const TOP: Self = Self { position: 0, muted: false };
}

/// Why a path has no master.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoMaster {
    /// No amplifier on the path has a gain range.
    NoGain,
    /// Neither an output pin nor the converter has a mute bit.
    NoMute,
}

/// The master `path` carries, or which half it lacks.
pub fn master(path: &OutputPath) -> Result<Master, NoMaster> {
    let pin_amps = || pins(path).filter_map(|pin| pin.amp.map(|amp| (pin.node, amp)));
    let amps = path.converter_amp.map(|amp| (path.converter, amp)).into_iter().chain(pin_amps());
    let (gain, range) = amps
        .filter_map(|(node, amp)| amp.gain.map(|range| (node, range)))
        .next()
        .ok_or(NoMaster::NoGain)?;
    let mut mute: Vec<Node> = pin_amps().filter(|(_, amp)| amp.mute).map(|(node, _)| node).collect();
    if mute.is_empty() && path.converter_amp.is_some_and(|amp| amp.mute) {
        mute.push(path.converter);
    }
    if mute.is_empty() {
        return Err(NoMaster::NoMute);
    }
    Ok(Master { codec: path.codec, gain, range, mute })
}

impl Master {
    /// Amplifier steps per position: 3 dB rounded up to whole steps.
    fn per_position(&self) -> u8 {
        POSITION_QUARTER_DB.div_ceil(self.range.step_size_quarter_db)
    }

    /// The lowest position, the amplifier's minimum.
    pub fn bottom(&self) -> u8 {
        self.range.zero_db.div_ceil(self.per_position())
    }

    /// `setting` moved by `delta` positions, clamped to the range, and
    /// unmuted: a step while muted unmutes.
    pub fn step(&self, setting: Setting, delta: i32) -> Setting {
        let position = (setting.position as i32).saturating_add(delta).clamp(0, self.bottom() as i32) as u8;
        Setting { position, muted: false }
    }

    fn index(&self, position: u8) -> u8 {
        self.range.zero_db.saturating_sub(position.min(self.bottom()) * self.per_position())
    }

    /// The verbs that set `setting`, gain and mute both, on every amplifier
    /// the master names.
    pub fn set(&self, setting: Setting) -> Vec<Verb> {
        let amp = |node: Node, gain: u8, muted: bool| {
            let mute = if muted { AMP_SET_MUTE } else { 0 };
            Verb::long(
                self.codec,
                node,
                verb::SET_AMP_GAIN_MUTE as u8,
                AMP_SET_OUTPUT | AMP_SET_LEFT | AMP_SET_RIGHT | mute | gain as u16,
            )
        };
        let gain = self.index(setting.position);
        let mut out = alloc::vec![amp(self.gain, gain, setting.muted && self.mute.contains(&self.gain))];
        for &node in self.mute.iter().filter(|&&node| node != self.gain) {
            out.push(amp(node, 0, setting.muted));
        }
        out
    }

    /// The verbs that read the master back: the gain amplifier, then every
    /// mute amplifier that is not it.
    pub fn read(&self) -> Vec<Verb> {
        let get = |node: Node| {
            Verb::long(self.codec, node, verb::GET_AMP_GAIN_MUTE as u8, AMP_GET_OUTPUT_LEFT)
        };
        core::iter::once(self.gain)
            .chain(self.mute.iter().copied().filter(|&node| node != self.gain))
            .map(get)
            .collect()
    }

    /// The setting the answers to [`Self::read`] say, or `None` where they
    /// name no setting: a count that is not the read's, a gain above 0 dB or
    /// between two positions, or mute amplifiers that disagree.
    pub fn decode(&self, answers: &[Response]) -> Option<Setting> {
        let [gain, rest @ ..] = answers else { return None };
        if answers.len() != self.read().len() {
            return None;
        }
        let index = (gain.raw() & AMP_ANSWER_GAIN) as u8;
        let below = self.range.zero_db.checked_sub(index)?;
        let position = below.div_ceil(self.per_position());
        if self.index(position) != index {
            return None;
        }
        let mute_answers: Vec<bool> = if self.mute.contains(&self.gain) {
            core::iter::once(gain).chain(rest).map(|a| a.raw() & AMP_ANSWER_MUTE != 0).collect()
        } else {
            rest.iter().map(|a| a.raw() & AMP_ANSWER_MUTE != 0).collect()
        };
        let muted = mute_answers[0];
        if mute_answers.iter().any(|&m| m != muted) {
            return None;
        }
        Some(Setting { position, muted })
    }
}

fn group_of<'a>(codecs: &'a [Codec], path: &OutputPath) -> Option<&'a FunctionGroup> {
    codecs
        .iter()
        .find(|c| c.address == path.codec)?
        .groups
        .iter()
        .find(|g| g.node == path.group)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;
    use crate::path::find_output_path;
    use toyos_sound_output::RATES as DEVICE_RATES;

    /// The bit this sequence never sets.
    const AMP_MUTE: u16 = AMP_SET_MUTE;

    fn laptop() -> (Vec<Codec>, OutputPath) {
        let codecs = fixture::laptop();
        let path = find_output_path(&codecs).unwrap();
        (codecs, path)
    }

    #[test]
    fn the_laptop_s_speaker_is_told_to_power_its_external_amplifier() {
        // Both output pins report EAPD capable and read the bit
        // back clear at boot, so a path configured without this is correct and
        // silent. Removing the `pin.eapd` arm reds here and nowhere else.
        let (codecs, path) = laptop();
        let sent = verbs(&codecs, &path, 0x4011, 1).unwrap();
        let eapd: Vec<u32> = sent
            .iter()
            .map(|v| v.raw())
            .filter(|v| (v >> 8) & 0xFFF == verb::SET_EAPD as u32)
            .collect();
        // The speaker at 0x14 and the jack at 0x21, both of them.
        assert_eq!(eapd, [0x0147_0C02, 0x0217_0C02]);
    }

    #[test]
    fn the_two_halves_of_the_laptop_s_volume_control_are_written_where_they_exist() {
        // Mute on the pin, gain on the converter, and neither
        // widget implements the other's field.
        let (codecs, path) = laptop();
        let sent = verbs(&codecs, &path, 0x4011, 1).unwrap();
        let amps: Vec<u32> = sent
            .iter()
            .map(|v| v.raw())
            .filter(|v| (v >> 16) & 0xF == verb::SET_AMP_GAIN_MUTE as u32 && v >> 28 == 0)
            .collect();
        // Pin 0x14 and pin 0x21: unmuted, gain field zero because there is no
        // gain range to index. Converter 0x02: the codec's own 0 dB index, 87.
        assert_eq!(amps, [0x0143_B000, 0x0213_B000, 0x0023_B057]);
        assert_eq!(path.converter_amp.unwrap().gain.unwrap().zero_db, 0x57);
    }

    #[test]
    fn nothing_in_the_sequence_ever_sets_the_mute_bit() {
        // The whole sequence exists to make a path audible. A mute here is the
        // defect that looks exactly like a driver that never ran.
        let (codecs, path) = laptop();
        for verb in verbs(&codecs, &path, 0x4011, 1).unwrap() {
            let raw = verb.raw();
            if (raw >> 16) & 0xF == verb::SET_AMP_GAIN_MUTE as u32 {
                assert_eq!(raw as u16 & AMP_MUTE, 0, "{raw:#010x} mutes an amplifier");
            }
        }
    }

    #[test]
    fn the_jack_is_told_to_drive_headphones_and_the_speaker_is_not() {
        let (codecs, path) = laptop();
        let sent = verbs(&codecs, &path, 0x4011, 1).unwrap();
        let control: Vec<u32> = sent
            .iter()
            .map(|v| v.raw())
            .filter(|v| (v >> 8) & 0xFFF == verb::SET_PIN_CONTROL as u32)
            .collect();
        assert_eq!(control, [0x0147_0740, 0x0217_07C0]);
    }

    #[test]
    fn the_converter_learns_the_format_and_the_tag_last() {
        let (codecs, path) = laptop();
        let sent = verbs(&codecs, &path, 0x4011, 3).unwrap();
        let tail: Vec<u32> = sent[sent.len() - 2..].iter().map(|v| v.raw()).collect();
        // Set Converter Format on node 0x02 with 44.1 kHz S16 stereo, then
        // stream tag 3 channel 0.
        assert_eq!(tail, [0x0022_4011, 0x0027_0630]);
    }

    #[test]
    fn every_widget_on_the_path_that_has_a_power_state_is_told_d0() {
        let (codecs, path) = laptop();
        let sent = verbs(&codecs, &path, 0x4011, 1).unwrap();
        let powered: Vec<u8> = sent
            .iter()
            .map(|v| v.raw())
            .filter(|v| (v >> 8) & 0xFFF == verb::SET_POWER_STATE as u32)
            .map(|v| ((v >> 20) & 0xFF) as u8)
            .collect();
        // The function group, the converter, the speaker and the jack. Both
        // pins carry a power state on this codec and both routes are depth 1,
        // so there is no hop between them.
        assert_eq!(powered, [0x01, 0x02, 0x14, 0x21]);
    }

    #[test]
    fn a_hop_shared_by_two_pins_is_powered_once() {
        // The laptop cannot produce this: both its routes are empty. The synthetic
        // selector graph is the only place a hop exists at all.
        let codecs = fixture::synthetic_selector();
        let path = find_output_path(&codecs).unwrap();
        let sent = verbs(&codecs, &path, 0x4011, 1).unwrap();
        let selects: Vec<u32> = sent
            .iter()
            .map(|v| v.raw())
            .filter(|v| (v >> 8) & 0xFFF == verb::SET_CONNECTION_SELECT as u32)
            .collect();
        assert_eq!(selects, [0x0207_0100, 0x0307_0101]);
    }

    /// The laptop's path with its converter's PCM word replaced by `word`.
    fn converter_offering(word: u32) -> (Vec<Codec>, OutputPath) {
        let mut codecs = fixture::laptop();
        let group = &mut codecs[0].groups[0];
        let dac = group.widgets.iter_mut().find(|w| w.node == Node(0x02)).unwrap();
        dac.pcm = Some(PcmCaps::decode(crate::verb::Response::new(word).unwrap()));
        let path = find_output_path(&codecs).unwrap();
        (codecs, path)
    }

    #[test]
    fn a_converter_offering_44100_runs_at_44100_as_both_machines_do() {
        let (codecs, path) = laptop();
        assert_eq!(rate(&codecs, &path, &DEVICE_RATES), Ok(44_100));
        assert_eq!(format(&codecs, &path, 44_100), Some((0x4011, 2)));

        let codecs = fixture::qemu();
        let path = find_output_path(&codecs).unwrap();
        assert_eq!(rate(&codecs, &path, &DEVICE_RATES), Ok(44_100));
        assert_eq!(format(&codecs, &path, 44_100), Some((0x4011, 2)));
    }

    /// The AMD bench's analog controller's codecs and the path chosen on them.
    fn amd_bench() -> (Vec<Codec>, OutputPath) {
        let codecs = fixture::amd_bench().swap_remove(1);
        let path = crate::path::find_output_path(&codecs).unwrap();
        (codecs, path)
    }

    #[test]
    fn the_amd_bench_s_converter_offering_48000_alone_runs_at_48000() {
        // Its own word is `0x000e0040`: 48 kHz at 16, 20 and 24 bits. The
        // recorded boot read `format 0x0011 (48000 Hz 2 ch 16-bit)` and `path
        // configured in 14 verbs, stream tag 1`.
        let (codecs, path) = amd_bench();
        assert_eq!(rate(&codecs, &path, &DEVICE_RATES), Ok(48_000));
        assert_eq!(format(&codecs, &path, 48_000), Some((0x0011, 2)));
        assert_eq!(verbs(&codecs, &path, 0x0011, 1).unwrap().len(), 14);
    }

    #[test]
    fn the_amd_bench_s_master_gains_on_the_converter_and_mutes_on_both_pins() {
        let (_, path) = amd_bench();
        let master = master(&path).unwrap();
        // The converter's amplifier is 88 steps of 0.75 dB with 0 dB at 87,
        // and both pins' are mute-only.
        assert_eq!(master.bottom(), 22);
        let raw: Vec<u32> = master.set(Setting { position: 1, muted: true }).iter().map(|v| v.raw()).collect();
        assert_eq!(raw, [0x0023_B053, 0x0143_B080, 0x0213_B080]);
    }

    #[test]
    fn a_converter_offering_48000_beside_others_but_not_44100_runs_at_48000() {
        // 32, 48 and 96 kHz at 16 bits.
        let (codecs, path) = converter_offering(0x0002_0150);
        assert_eq!(rate(&codecs, &path, &DEVICE_RATES), Ok(48_000));
    }

    #[test]
    fn a_converter_offering_neither_rate_is_refused_naming_what_it_offers() {
        // 32 and 96 kHz at 16 bits.
        let (codecs, path) = converter_offering(0x0002_0110);
        let Err(Some(pcm)) = rate(&codecs, &path, &DEVICE_RATES) else {
            panic!("a converter offering neither rate was given one");
        };
        assert_eq!(pcm.rates().collect::<Vec<_>>(), [32_000, 96_000]);
        assert_eq!(format(&codecs, &path, 44_100), None);
        assert_eq!(format(&codecs, &path, 48_000), None);
    }

    /// What a codec answers a `Get Amplifier Gain/Mute` with after `verbs`:
    /// the gain and mute each amplifier was last set to, left channel.
    fn answers(master: &Master, sent: &[Verb]) -> Vec<Response> {
        master
            .read()
            .iter()
            .map(|get| {
                let node = (get.raw() >> 20) & 0xFF;
                let set = sent.iter().rev().find(|v| (v.raw() >> 20) & 0xFF == node).unwrap();
                Response::new(set.raw() & 0xFF).unwrap()
            })
            .collect()
    }

    #[test]
    fn the_laptop_s_master_gains_on_the_converter_and_mutes_on_both_pins() {
        let (_, path) = laptop();
        let master = master(&path).unwrap();
        // 88 steps of 0.75 dB, 0 dB at 87: four steps a position, 22 positions down.
        assert_eq!(master.bottom(), 22);
        let raw: Vec<u32> = master.set(Setting { position: 1, muted: true }).iter().map(|v| v.raw()).collect();
        assert_eq!(raw, [0x0023_B053, 0x0143_B080, 0x0213_B080]);
        let read: Vec<u32> = master.read().iter().map(|v| v.raw()).collect();
        assert_eq!(read, [0x002B_A000, 0x014B_A000, 0x021B_A000]);
    }

    #[test]
    fn qemus_master_gains_and_mutes_on_its_converter() {
        let codecs = fixture::qemu();
        let path = find_output_path(&codecs).unwrap();
        let master = master(&path).unwrap();
        let raw: Vec<u32> = master.set(Setting { position: 2, muted: true }).iter().map(|v| v.raw()).collect();
        // 0 dB at 74, 1 dB steps: three steps a position.
        assert_eq!(raw, [0x0023_B0C4]);
        assert_eq!(master.read().len(), 1);
    }

    #[test]
    fn every_setting_reads_back_as_itself() {
        for (_, path) in [laptop(), {
            let codecs = fixture::qemu();
            let path = find_output_path(&codecs).unwrap();
            (codecs, path)
        }] {
            let master = master(&path).unwrap();
            for position in 0..=master.bottom() {
                for muted in [false, true] {
                    let setting = Setting { position, muted };
                    assert_eq!(master.decode(&answers(&master, &master.set(setting))), Some(setting));
                }
            }
        }
    }

    #[test]
    fn a_step_is_clamped_to_the_range_and_unmutes() {
        let (_, path) = laptop();
        let master = master(&path).unwrap();
        let muted_top = Setting { position: 0, muted: true };
        assert_eq!(master.step(muted_top, -1), Setting::TOP);
        assert_eq!(master.step(Setting::TOP, 1), Setting { position: 1, muted: false });
        let bottom = Setting { position: master.bottom(), muted: false };
        assert_eq!(master.step(bottom, 1), bottom);
        assert_eq!(master.step(Setting::TOP, i32::MAX), bottom);
        assert_eq!(master.step(bottom, i32::MIN), Setting::TOP);
        // The bottom is the amplifier's minimum, index 0, not past it.
        assert_eq!(master.set(bottom)[0].raw() & 0x7F, 0);
    }

    #[test]
    fn answers_that_name_no_setting_are_refused() {
        let (_, path) = laptop();
        let master = master(&path).unwrap();
        let r = |raw| Response::new(raw).unwrap();
        // Above 0 dB, between two positions, the pins disagreeing, and one answer short.
        assert_eq!(master.decode(&[r(0x58), r(0), r(0)]), None);
        assert_eq!(master.decode(&[r(0x56), r(0), r(0)]), None);
        assert_eq!(master.decode(&[r(0x57), r(0x80), r(0)]), None);
        assert_eq!(master.decode(&[r(0x57), r(0)]), None);
        assert_eq!(master.decode(&[r(0x57), r(0x80), r(0x80)]), Some(Setting { position: 0, muted: true }));
    }

    #[test]
    fn a_path_with_no_gain_range_or_no_mute_bit_has_no_master() {
        let (_, mut path) = laptop();
        let converter = path.converter_amp.take();
        assert_eq!(master(&path), Err(NoMaster::NoGain));
        path.converter_amp = converter;
        path.output.amp = None;
        path.headphone.as_mut().unwrap().amp = None;
        assert_eq!(master(&path), Err(NoMaster::NoMute));
    }

    #[test]
    fn qemus_path_writes_its_amplifier_on_the_converter_and_none_on_the_pin() {
        let codecs = fixture::qemu();
        let path = find_output_path(&codecs).unwrap();
        let sent = verbs(&codecs, &path, 0x4011, 1).unwrap();
        let amps: Vec<u32> = sent
            .iter()
            .map(|v| v.raw())
            .filter(|v| (v >> 16) & 0xF == verb::SET_AMP_GAIN_MUTE as u32)
            .collect();
        assert_eq!(amps, [0x0023_B04A]);
    }
}
