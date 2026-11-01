//! `screen`: what the display shows, and its mode or scale set, over the
//! compositor's `display` port.
//!
//! ```text
//! screen                     the mode and the scale
//! screen WIDTHxHEIGHT        take this mode
//! screen scale 1.25|auto     draw at this scale, kept across boots; or the panel's
//! ```

use toyos_manifest::display::PORT;
use window::display::{self, Setting, Shown};
use window::Scale;

const USAGE: &str = "usage: screen [WIDTHxHEIGHT | scale SCALE | scale auto]";

pub fn main(args: Vec<String>) {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let shown = match args[..] {
        [] => display::get(PORT),
        ["scale", "auto"] => display::set_scale(PORT, Setting::Auto),
        ["scale", scale] => match scale.parse::<Scale>() {
            Ok(scale) => display::set_scale(PORT, Setting::Kept(scale)),
            Err(why) => fail(&format!("{scale:?} is no scale: {why}")),
        },
        [mode] => match mode.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))) {
            Some((width, height)) => display::set_mode(PORT, width, height),
            None => fail(&format!("{mode:?} is no mode\n{USAGE}")),
        },
        _ => fail(USAGE),
    };
    match shown {
        Ok(shown) => say(shown),
        Err(e @ display::Error::NotEndowed) => fail(&format!("{e}; run it with `launch screen`")),
        Err(e) => fail(&e.to_string()),
    }
}

fn say(shown: Shown) {
    let setting = match shown.setting {
        Setting::Auto => "auto".to_string(),
        Setting::Kept(scale) => format!("kept {scale}"),
    };
    println!("{}x{} scale {} ({setting})", shown.width, shown.height, shown.scale);
}

fn fail(why: &str) -> ! {
    eprintln!("screen: {why}");
    std::process::exit(1);
}
