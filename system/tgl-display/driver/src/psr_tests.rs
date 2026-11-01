//! AUX transactions and PSR1, against the model's panel.

extern crate std;

use crate::model::Engine;
use crate::{aux, keep_memory_low_power, psr, regs, Refusal};

#[test]
fn a_native_read_and_write_reach_the_panels_dpcd() {
    let engine = Engine::t14();
    let mut caps = [0u8; 2];
    aux::read(&engine, &engine, 0x070, &mut caps).expect("read");
    assert_eq!(caps, [0x01, 0x00]);
    aux::write(&engine, &engine, 0x600, &[0x01]).expect("written");
    assert_eq!(*engine.panel.written.borrow(), [(0x600, 0x01)]);
    // The request went out most significant byte first: native read, address
    // 00070h, two bytes.
    let read = engine.written().into_iter().find(|&(at, _)| at == regs::DDI_AUX_DATA_A).expect("a request").1;
    assert_eq!(read, 0x9000_7001);
}

#[test]
fn a_deferring_panel_is_asked_again_and_a_silent_one_is_refused() {
    let engine = Engine::t14();
    engine.panel.defers.set(3);
    let mut version = [0u8];
    aux::read(&engine, &engine, 0x070, &mut version).expect("answered after three defers");
    assert_eq!(version, [0x01]);
    engine.panel.silent.set(true);
    assert_eq!(aux::read(&engine, &engine, 0x070, &mut version), Err(aux::Fault::Timeout));
    engine.panel.silent.set(false);
    engine.panel.defers.set(u32::MAX);
    assert_eq!(aux::read(&engine, &engine, 0x070, &mut version), Err(aux::Fault::Defer));
}

/// The panel read, then both sides prepared.
fn both(engine: &Engine, done: &crate::Programmed) -> Result<psr::Prepared, Refusal> {
    psr::read_sink(engine, engine).and_then(|sink| psr::prepare(engine, engine, done, sink))
}

fn programmed(engine: &Engine) -> crate::Programmed {
    keep_memory_low_power(engine, engine).expect("the T14's GOP is programmed")
}

#[test]
fn the_t14_panel_is_told_the_link_stays_active_then_enabled_and_srd_follows() {
    let engine = Engine::t14();
    let done = programmed(&engine);
    let prepared = both(&engine, &done).expect("prepared");
    assert_eq!((prepared.setup_lines, prepared.vblank_lines), (23, 32), "330 µs at 15 µs a line, in a 32-line blank");
    assert_eq!(*engine.panel.written.borrow(), [(0x170, 0b10), (0x170, 0b11), (0x600, 0x01)]);
    assert_eq!(engine.get(regs::VIDEO_DIP_CTL_A) & (regs::VSC_SELECT | regs::VSC_ENABLE), regs::VSC_ENABLE);
    assert_eq!(engine.get(regs::SRD_CTL_A), 0, "not before activate");
    let psr = prepared.activate(&engine);
    assert_eq!(engine.get(regs::SRD_CTL_A), 0x81f0_02e6, "enable, max sleep, TP2 and TP1 2.5 ms, TP4 skipped, 6 idle frames");
    let surface = regs::plane_surf(0, 1);
    let before = engine.written().len();
    psr.update(&engine);
    assert!(engine.written()[before..].contains(&(surface, engine.get(surface))), "the surface flipped to itself");
}

#[test]
fn a_panel_without_psr_or_with_errors_is_left_unconfigured() {
    for (at, value, why) in [
        (0x070, 0x00, Refusal::NoPsr),
        (0x701, 0x00, Refusal::NoSetPower),
        (0x071, 0b1110, Refusal::SinkSetupReserved),
        (0x2006, 0b001, Refusal::SinkErrors { errors: 1, status: 0 }),
        (0x2008, 0b111, Refusal::SinkErrors { errors: 0, status: 7 }),
    ] {
        let engine = Engine::t14();
        let done = programmed(&engine);
        engine.panel.dpcd.borrow_mut().insert(at, value);
        assert_eq!(both(&engine, &done).map(|_| ()), Err(why));
        assert!(engine.panel.written.borrow().is_empty(), "{why:?}: the panel is not configured");
        assert_eq!(engine.get(regs::SRD_CTL_A), 0);
    }
}

#[test]
fn a_setup_time_past_vertical_blank_is_refused() {
    let engine = Engine::t14();
    let done = programmed(&engine);
    // A 1100-line total leaves a 20-line blank: 330 µs is 23 lines.
    engine.set(regs::TRANS_VTOTAL_A, 0x044b_0437);
    let shorter = crate::Programmed { scanout: crate::Scanout { vtotal: 1100, ..done.scanout }, ..done };
    assert_eq!(both(&engine, &shorter).map(|_| ()), Err(Refusal::SetupTime { lines: 23, vblank: 20 }));
}

/// PSR already on is refused before any AUX; what else the firmware lacks
/// is refused before the panel is configured.
#[test]
fn what_psr_needs_and_the_firmware_lacks_is_refused_before_the_panel_is_configured() {
    let engine = Engine::t14();
    engine.set(regs::SRD_CTL_A, regs::SRD_ENABLE);
    assert_eq!(psr::read_sink(&engine, &engine).map(|_| ()), Err(Refusal::PsrOn));
    assert!(!engine.written().iter().any(|&(at, _)| at == regs::DDI_AUX_CTL_A), "no AUX");
    let refused = |engine: &Engine, why: Refusal| {
        let done = programmed(engine);
        assert_eq!(both(engine, &done).map(|_| ()), Err(why));
        assert!(engine.panel.written.borrow().is_empty(), "{why:?}: the panel is not configured");
    };
    let engine = Engine::t14();
    engine.set(regs::PSR_IIR_A, regs::PSR_AUX_ERROR);
    refused(&engine, Refusal::PsrAuxError);
    let engine = Engine::t14();
    engine.set(regs::TRANS_VBLANK_A, 0x0457_0439);
    refused(&engine, Refusal::DelayedVblank { start: 1082, active: 1080 });
    let engine = Engine::t14();
    engine.panel.dpcd.borrow_mut().insert(0x000, 0);
    refused(&engine, Refusal::NoReceiver);
}

#[test]
fn the_source_is_awake_only_out_of_srd_and_an_aux_error_disables_it() {
    let engine = Engine::t14();
    let done = programmed(&engine);
    let psr = both(&engine, &done).expect("prepared").activate(&engine);
    for (state, awake) in [(0b000, true), (0b001, true), (0b010, false), (0b111, false), (0b101, false)] {
        engine.set(regs::SRD_STATUS_A, state << regs::SRD_STATE_SHIFT);
        assert_eq!(psr.awake(&engine), awake, "state {state:#05b}");
    }
    assert_eq!(psr.fault(&engine), None);
    engine.set(regs::PSR_IIR_A, regs::PSR_AUX_ERROR);
    assert_eq!(psr.fault(&engine), Some(Refusal::PsrAuxError));
    assert_eq!(engine.get(regs::SRD_CTL_A) & regs::SRD_ENABLE, 0);
}

#[test]
fn disabling_waits_for_idle_says_whether_it_came_and_can_enable_again() {
    let engine = Engine::t14();
    let done = programmed(&engine);
    let enable = || both(&engine, &done).expect("prepared").activate(&engine);
    engine.set(regs::SRD_STATUS_A, 0);
    let (idle, again) = enable().disable(&engine, &engine, 16_666_667);
    assert!(idle);
    assert_eq!(engine.get(regs::SRD_CTL_A) & regs::SRD_ENABLE, 0);
    // The panel stays configured: enabling again writes SRD_CTL alone.
    let written = engine.panel.written.borrow().len();
    again.activate(&engine);
    assert_ne!(engine.get(regs::SRD_CTL_A) & regs::SRD_ENABLE, 0);
    assert_eq!(engine.panel.written.borrow().len(), written, "no AUX on enabling again");
    engine.set(regs::SRD_CTL_A, 0);
    engine.panel.written.borrow_mut().clear();
    engine.set(regs::SRD_STATUS_A, 0b010 << regs::SRD_STATE_SHIFT);
    assert!(!enable().disable(&engine, &engine, 16_666_667).0, "a source stuck in SRD is said");
}
