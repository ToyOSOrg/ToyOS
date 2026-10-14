//! The receive reordering buffer against §10.25.6's rules, one rule per test.

use toyos_ieee80211::reorder::Window;
use toyos_ieee80211::{Refused, Seq};

fn window(start: u16, size: u16) -> Window<u16> {
    Window::new(Seq::new(start), size).unwrap()
}

fn receive(w: &mut Window<u16>, seq: u16) -> Vec<u16> {
    let mut out = Vec::new();
    w.receive(Seq::new(seq), seq, &mut out).unwrap();
    out
}

#[test]
fn frames_in_order_pass_up_as_they_arrive() {
    let mut w = window(10, 8);
    assert_eq!(receive(&mut w, 10), [10]);
    assert_eq!(receive(&mut w, 11), [11]);
    assert_eq!(w.start(), Seq::new(12));
}

#[test]
fn a_hole_holds_the_frames_after_it_until_it_fills() {
    let mut w = window(10, 8);
    assert_eq!(receive(&mut w, 12), []);
    assert_eq!(receive(&mut w, 11), []);
    assert_eq!(w.first_held(), Some(Seq::new(11)));
    assert_eq!(receive(&mut w, 10), [10, 11, 12]);
    assert_eq!(w.first_held(), None);
}

#[test]
fn a_frame_past_the_end_moves_the_window_to_end_at_it() {
    let mut w = window(10, 8);
    assert_eq!(receive(&mut w, 12), []);
    assert_eq!(receive(&mut w, 14), []);
    // 20 ends the window at 20, so it starts at 13: 12 passes up, then the run 13 lacks.
    assert_eq!(receive(&mut w, 20), [12]);
    assert_eq!(w.start(), Seq::new(13));
    assert_eq!(receive(&mut w, 13), [13, 14]);
}

#[test]
fn the_last_slot_of_the_window_moves_nothing() {
    let mut w = window(10, 8);
    assert_eq!(receive(&mut w, 17), []);
    assert_eq!(w.start(), Seq::new(10));
}

#[test]
fn a_frame_a_whole_window_past_passes_up_everything_held() {
    let mut w = window(10, 4);
    receive(&mut w, 11);
    receive(&mut w, 13);
    assert_eq!(receive(&mut w, 2000), [11, 13]);
    assert_eq!(w.start(), Seq::new(1997));
}

#[test]
fn a_frame_half_the_sequence_space_behind_is_old() {
    let mut w = window(10, 8);
    let mut out = Vec::new();
    assert_eq!(w.receive(Seq::new(9), 9, &mut out), Err(Refused::OldSequence));
    assert_eq!(w.receive(Seq::new(10 + 2048), 0, &mut out), Err(Refused::OldSequence));
    assert_eq!(receive(&mut w, 10 + 2047), []);
}

#[test]
fn a_release_passes_up_what_is_held_before_it_and_the_run_from_it() {
    let mut w = window(10, 8);
    receive(&mut w, 11);
    receive(&mut w, 13);
    receive(&mut w, 14);
    let mut out = Vec::new();
    w.release_to(Seq::new(13), &mut out);
    assert_eq!(out, [11, 13, 14]);
    assert_eq!(w.start(), Seq::new(15));
}

#[test]
fn a_release_at_or_behind_the_start_changes_nothing() {
    let mut w = window(10, 8);
    receive(&mut w, 12);
    let mut out = Vec::new();
    w.release_to(Seq::new(10), &mut out);
    w.release_to(Seq::new(5), &mut out);
    assert_eq!((out, w.start()), (vec![], Seq::new(10)));
}

#[test]
fn the_window_runs_across_the_end_of_the_sequence_space() {
    let mut w = window(4094, 8);
    assert_eq!(receive(&mut w, 1), []);
    assert_eq!(receive(&mut w, 4095), []);
    assert_eq!(receive(&mut w, 0), []);
    assert_eq!(receive(&mut w, 4094), [4094, 4095, 0, 1]);
    assert_eq!(w.start(), Seq::new(2));
}

#[test]
fn a_window_of_one_to_two_hundred_fifty_six_is_made_and_no_other() {
    assert!(Window::<u16>::new(Seq::new(0), 1).is_ok());
    assert!(Window::<u16>::new(Seq::new(0), 256).is_ok());
    assert_eq!(Window::<u16>::new(Seq::new(0), 0).err(), Some(Refused::WindowSize));
}

#[test]
fn an_he_window_holds_two_hundred_fifty_six_frames_behind_a_hole() {
    let mut w = window(4000, 256);
    for seq in 4001..4096 {
        assert_eq!(receive(&mut w, seq), []);
    }
    for seq in 0..160 {
        assert_eq!(receive(&mut w, seq), []);
    }
    let all: Vec<u16> = (4000..4096).chain(0..160).collect();
    assert_eq!(receive(&mut w, 4000), all);
}
