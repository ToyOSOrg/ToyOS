//! One run of the compositor: the devices it claimed, the clients it holds and
//! the screen it owns.
//!
//! Every method here is an effect. The decisions they act on come from
//! `toyos_desktop` and are host-tested there; what is left is claiming the
//! devices, draining the client connections without ever blocking on one,
//! allocating and granting the shared buffers, and handing finished rectangles
//! to the panel.
//!
//! **No clipboard byte is ever logged**: a refusal names a length and a client.

use std::time::{Duration, Instant};

use toyos::endow;
use toyos::ipc::{Connection, FrameRx, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::port::Acceptor;
use toyos::shm::{ReadOnly, SharedMemory};
use toyos::{ipc, system, AsHandle};
use toyos_abi::input::RawKeyEvent;
use toyos_abi::RawHandle;
use toyos_input::wire::{self, Record, CONNECTIONS, MAX_PAYLOAD};
use toyos_input::wire::DEVICES;
use toyos_input::{Full, Merge, Pointer, Slot, State};
use toyos_desktop::clipboard::{Landing, Watch};
use toyos_desktop::volume::{overlay_rect, Overlay, Request, Requests, Shown};
use toyos::audio::{Volume, VolumeStep, MSG_VOLUME, MSG_VOLUME_MUTE, MSG_VOLUME_STEP, MSG_VOLUME_UNAVAILABLE};
use toyos_desktop::{
    cursor, cursor_from_abs, cursor_style, fold_mouse, hit_test, key_action, relayout, set_mode, sign_out_key,
    sign_out_rect, sign_out_text, tab_action, SignOutKey,
    Buffers, CursorStyle, Presented, Requested, Damage, Desk, Scale, Grab, Held, Hit, KeyAction, Level, Mode, Point, Port, PromptGate,
    Rect, Released, Stack, TabAction, Verdict, Window, WindowId, WindowMode,
};
use toyos_icon::{Icon, Mask};
use window::display::{self, Refused, Setting};
use window::{Framebuffer, Screen};

use crate::client::{
    announce, deliver, deliver_signal, deliver_with_handle, mark_dead, note_closed, note_opened,
    Buffer, Client, ClientFrame, ClientRx, CopyRegion, Dead, DropReason, PendingConn, Watcher, Win,
    HANDSHAKE_TIMEOUT, MAX_PENDING_CONNS,
};
use crate::display::Display;
use crate::render::{self, Assets, SystemStats, TitleBarIcons};
use crate::stats::FrameTotals;
use crate::{
    launcher_apps, CLIPBOARD_POLL_HANDLES, DISPLAY_OUTPUT_POLL_HANDLES, DISPLAY_PORT_POLL_HANDLES, DOUBLE_CLICK_TIME, DRAIN_BUDGET,
    FIXED_POLL_HANDLES, FRAME_INTERVAL, INPUT_POLL_HANDLES, MAX_WINDOW_SLOTS, PROMPT_POLL_HANDLES,
    VOLUME_POLL_HANDLES,
};

/// The most frames one source's turn takes before the others are served.
const SOURCE_FRAMES: usize = 64;

/// One connection on the `input` port: a source of keys and pointer reports.
struct Source {
    conn: Connection,
    rx: FrameRx<MAX_PAYLOAD>,
}

const _: () = assert!(
    toyos_inspect::MAX_SNAPSHOT_BYTES == ipc::MAX_FRAME_LEN as usize,
    "a snapshot is one frame"
);

/// The desktop's font and icons as the image ships them, which every scale
/// is drawn from.
struct Shapes {
    font: Vec<u8>,
    cursor: Icon,
    resize: Icon,
    crosshair: Icon,
    minimize: Icon,
    maximize: Icon,
    close: Icon,
}

/// Where the image carries the desktop's font, and its icons.
const FONT_PATH: &str = "/system/share/fonts/JetBrainsMono-Regular.ttf";
const ICONS_DIR: &str = "/system/share/icons";

/// A title-bar button's icon, in logical pixels.
const BUTTON_ICON: i32 = 14;

impl Shapes {
    fn read() -> Self {
        let icon = |stem: &str| {
            let path = format!("{ICONS_DIR}/{stem}.svg");
            let svg = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("compositor: {path}: {e}"));
            Icon::parse(&svg)
        };
        Self {
            font: std::fs::read(FONT_PATH).unwrap_or_else(|e| panic!("compositor: {FONT_PATH}: {e}")),
            cursor: icon("cursor-bold"),
            resize: icon("arrow-down-right-bold"),
            crosshair: icon("crosshair-simple-bold"),
            minimize: icon("minus-bold"),
            maximize: icon("square-bold"),
            close: icon("x-bold"),
        }
    }

    /// The font at `desk`'s text cell.
    fn font(&self, desk: &Desk) -> font::Font {
        let (w, h) = desk.cell();
        font::Font::from_ttf(&self.font, w, h)
            .unwrap_or_else(|e| panic!("compositor: {FONT_PATH}: {e}"))
    }

    fn buttons(&self, scale: Scale) -> TitleBarIcons {
        let edge = scale.px(BUTTON_ICON) as usize;
        TitleBarIcons {
            minimize: self.minimize.mask(edge),
            maximize: self.maximize.mask(edge),
            close: self.close.mask(edge),
        }
    }

    fn cursors(&self, scale: Scale) -> Cursors {
        let edge = cursor::edge(scale) as usize;
        Cursors {
            default: self.cursor.mask(edge),
            resize: self.resize.mask(edge),
            crosshair: self.crosshair.mask(edge),
        }
    }
}

/// The pointer's sprites at the desk's scale.
struct Cursors {
    default: Mask,
    crosshair: Mask,
    resize: Mask,
}

impl Cursors {
    /// `style`'s mask and the colour it is drawn in.
    fn get(&self, style: CursorStyle) -> (&Mask, [u8; 3]) {
        match style {
            CursorStyle::Crosshair => (&self.crosshair, [0, 0, 0]),
            CursorStyle::Resize => (&self.resize, [255, 255, 255]),
            CursorStyle::Default => (&self.default, [255, 255, 255]),
        }
    }
}

/// The file in the compositor's `HOME` that keeps the person's own scale,
/// set by `screen scale`: a decimal, or no file for the panel's.
const KEPT_SCALE: &str = "scale";

/// The wallpaper as it is stored: width, height, then RGB triples.
struct Wallpaper {
    raw: Vec<u8>,
    w: usize,
    h: usize,
    /// Scaled to the current screen and in its pixel format.
    scaled: Vec<u8>,
}

impl Wallpaper {
    fn rescale(&mut self, mode: Mode, pixel_format: u32) {
        self.scaled = render::scale_wallpaper(
            &self.raw[8..],
            self.w,
            self.h,
            mode.width as usize,
            mode.height as usize,
            pixel_format != 0,
        );
    }
}

/// The scale this image's own row starts the desktop at
/// ([`toyos_manifest::display::SCALE_ARG`]), if it names one; a word that is
/// no quarter step is refused by name.
fn image_scale() -> Option<Scale> {
    use toyos_manifest::display::SCALE_ARG;
    let word = std::env::args().skip(1).find_map(|a| a.strip_prefix(SCALE_ARG).map(str::to_string))?;
    let scale = word
        .parse::<window::Scale>()
        .map_err(|e| e.to_string())
        .and_then(|s| Scale::step(s).ok_or_else(|| Refused::NotAStep.to_string()));
    scale.map_err(|why| eprintln!("compositor: the image's {SCALE_ARG}{word} is refused: {why}")).ok()
}

/// Where the person's own scale is kept, in this program's `HOME`.
fn kept_path() -> std::path::PathBuf {
    std::env::home_dir().expect("the supervisor starts a service with HOME").join(KEPT_SCALE)
}

/// The person's own scale, as `screen scale` last kept it; a file that does
/// not hold one is refused by name.
fn kept_scale() -> Option<Scale> {
    let path = kept_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!("compositor: the kept scale in {} is not read, and is not drawn at: {e}", path.display());
            return None;
        }
    };
    match text.trim().parse::<window::Scale>().ok().and_then(Scale::step) {
        Some(scale) => Some(scale),
        None => {
            eprintln!("compositor: the kept scale {:?} is no quarter step, and is not drawn at", text.trim());
            None
        }
    }
}

/// What `mode` is drawn at ([`Scale::shown`]); a scale the mode cannot hold
/// is said by name, and stays what was asked for.
fn scale_for(kept: Option<Scale>, base: Scale, mode: Mode) -> Scale {
    let asked = kept.unwrap_or(base);
    let scale = Scale::shown(kept, base, (mode.width, mode.height));
    if scale != asked {
        println!("compositor: scale {asked} leaves {mode} less than the least screen; drawing at {scale}, {asked} still asked");
    }
    scale
}

pub struct Session {
    acceptor: Acceptor,
    /// The prompt layer's port ([`toyos_manifest::consent::PROMPT`]), which
    /// one program alone holds: a window made on it goes above every other.
    prompts: Acceptor,
    /// The prompt while one is up, and which of the input reaches it.
    prompt: Option<(WindowId, PromptGate)>,
    /// The last prompt's gate once it is gone: the releases of what it was
    /// given, which no other window saw the press of.
    owed: PromptGate,
    /// The clipboard's port ([`toyos_manifest::clipboard::PORT`]), which one
    /// program alone holds: its one connection is sent every copy.
    clipboards: Acceptor,
    /// The display's settings' port ([`toyos_manifest::display::PORT`]), which
    /// one program alone holds.
    settings: Acceptor,
    /// The `display-output` port, and the display driver on it.
    outputs: Acceptor,
    output: Option<crate::output::Driver>,
    watcher: Option<Watcher>,
    /// What the watcher has asked and is sent, for the compositor's whole life:
    /// a copy made while no watcher is there is the next one's.
    watch: Watch<CopyRegion>,
    /// The `input` port, and the sources on it, by their slot.
    inputs: Acceptor,
    sources: [Option<Source>; CONNECTIONS],
    /// Every source's keys and pointers.
    merge: Merge,
    pointers: Pointer,
    poller: Poller,

    display: Display,
    /// The scale this image's row starts the desktop at, where it names one.
    image: Option<Scale>,
    /// The scale the panel's EDID gives the mode on screen.
    panel: Scale,
    /// The person's own scale, kept across boots; drawn at as far as the
    /// mode holds it ([`Scale::shown`]).
    kept: Option<Scale>,
    back: Framebuffer<'static>,
    cursors: Cursors,
    current_cursor: CursorStyle,

    shapes: Shapes,
    font: font::Font,
    icons: TitleBarIcons,
    wallpaper: Wallpaper,

    desk: Desk,
    stack: Stack<Client>,
    pending: Vec<PendingConn>,
    damage: Damage,
    cursor: Point,
    prev_buttons: u8,
    grab: Grab,
    last_click_handle: Option<RawHandle>,
    last_click_at: Instant,
    clipboard: String,
    launcher_open: bool,
    /// `(label, program)`, re-read from `/apps` every time the launcher opens.
    apps: Vec<(String, String)>,
    total_mem: u64,
    max_windows: usize,

    /// Clients to remove at the end of the pass that condemned them.
    dead: Vec<Dead>,
    /// Tokens the last `wait` reported ready.
    ready: Vec<u64>,

    /// soundserver's `volume` port, connected on the first volume key and
    /// again after it ends; the requests on it; and the overlay its answers show.
    volume: Option<(toyos::Connection, ipc::FrameRx<{ std::mem::size_of::<Volume>() }>)>,
    volume_requests: Requests,
    overlay: Overlay,
    /// What the overlay's clock counts from.
    epoch: Instant,

    totals: FrameTotals,
    cached_stats: SystemStats,
    prev_busy_ticks: u64,
    prev_total_ticks: u64,
    last_taskbar_update: Instant,
    /// The desktop session this compositor signs out and launches in.
    desktop: crate::signout::Desktop,
}

impl Session {
    pub fn start() -> Self {
        // Every one of these is a statement about the manifest the image was
        // built from, which `build/build.rs` checked before the image was
        // written — so `expect` is right here where `services::listen`'s was
        // not. There is no other process to have taken the name and no name to
        // take.
        let acceptor = endow::acceptor("compositor")
            .expect("the manifest declares this program serves `compositor`");
        let prompts = endow::acceptor(toyos_manifest::consent::PROMPT)
            .expect("the manifest declares this program serves `prompt`");
        let clipboards = endow::acceptor(toyos_manifest::clipboard::PORT)
            .expect("the manifest declares this program serves `clipboard`");
        let settings = endow::acceptor(toyos_manifest::display::PORT)
            .expect("the manifest declares this program serves `display`");
        let inputs = endow::acceptor(wire::PORT)
            .expect("the manifest declares this program serves `input`");
        let outputs = endow::acceptor(toyos_display_output::PORT)
            .expect("the manifest declares this program serves `display-output`");
        let (mut display, panel) = Display::claim();
        let desktop = crate::signout::Desktop::start();
        let mode = display.mode();
        let (image, kept) = (image_scale(), kept_scale());
        let scale = scale_for(kept, image.unwrap_or(panel), mode);
        match (kept, image) {
            (Some(k), _) => println!("compositor: drawing at scale {scale}, kept {k}"),
            (None, Some(_)) => println!("compositor: drawing at scale {scale}, the image's"),
            (None, None) => println!("compositor: drawing at scale {scale}"),
        }
        let screen = display.screen();
        let back = Framebuffer::new(screen.width(), screen.height(), screen.pixel_format_raw());

        let shapes = Shapes::read();
        let cursors = shapes.cursors(scale);

        let raw = std::fs::read("/system/share/wallpaper.rgb").expect("failed to read wallpaper");
        let mut wallpaper = Wallpaper {
            w: u32::from_le_bytes(raw[0..4].try_into().unwrap()) as usize,
            h: u32::from_le_bytes(raw[4..8].try_into().unwrap()) as usize,
            raw,
            scaled: Vec::new(),
        };
        let mode = display.mode();
        println!("compositor: wallpaper {}x{}, scaling to {mode}", wallpaper.w, wallpaper.h);
        wallpaper.rescale(mode, display.screen().pixel_format_raw());

        let icons = shapes.buttons(scale);

        let apps = launcher_apps();
        let desk = Desk::new(mode_rect(mode), scale, apps.len());
        let font = shapes.font(&desk);
        let total_mem = toyos::system::memory_total();
        let max_windows =
            toyos_desktop::max_windows(total_mem, desk.screen, MAX_WINDOW_SLOTS as usize);
        println!(
            "compositor: at most {max_windows} windows ({} MiB each of {} MiB total)",
            toyos_desktop::window_bytes(desk.screen) / (1024 * 1024),
            total_mem / (1024 * 1024),
        );

        // Sized for the slot ceiling rather than for `max_windows`: a mode
        // or a scale set on the display port can raise `max_windows` mid-run.
        let poller = Poller::new(
            FIXED_POLL_HANDLES
                + MAX_WINDOW_SLOTS
                + MAX_PENDING_CONNS
                + PROMPT_POLL_HANDLES
                + CLIPBOARD_POLL_HANDLES
                + DISPLAY_PORT_POLL_HANDLES
                + DISPLAY_OUTPUT_POLL_HANDLES
                + INPUT_POLL_HANDLES
                + VOLUME_POLL_HANDLES,
        );
        poller.watch(&inputs, READABLE, inputs.as_handle().raw().0 as u64);
        poller.watch(&acceptor, READABLE, acceptor.as_handle().raw().0 as u64);
        poller.watch(&prompts, READABLE, prompts.as_handle().raw().0 as u64);
        poller.watch(&clipboards, READABLE, clipboards.as_handle().raw().0 as u64);
        poller.watch(&settings, READABLE, settings.as_handle().raw().0 as u64);
        poller.watch(&outputs, READABLE, outputs.as_handle().raw().0 as u64);
        if let Some(handle) = display.handle() {
            poller.watch(&handle, READABLE, handle.raw().0 as u64);
        }
        if let Some(control) = desktop.conn() {
            poller.watch(control, READABLE, control.as_handle().raw().0 as u64);
        }
        poller.watch(desktop.wake(), READABLE, desktop.wake().as_handle().raw().0 as u64);

        let cursor = Point { x: desk.screen.w() / 2, y: desk.screen.h() / 2 };
        if display.hw_cursor() && cursor::in_hardware(scale) {
            let (mask, color) = cursors.get(CursorStyle::Default);
            let hot = cursor::hot_spot(CursorStyle::Default, scale);
            display.set_cursor(&render::cursor_image(mask, color), (hot.x as u32, hot.y as u32), (cursor.x as u32, cursor.y as u32));
        }
        let mut damage = Damage::default();
        damage.add(desk.screen);

        println!("compositor: ready");

        let now = Instant::now();
        Self {
            acceptor,
            prompts,
            prompt: None,
            owed: PromptGate::default(),
            clipboards,
            settings,
            outputs,
            output: None,
            watcher: None,
            watch: Watch::default(),
            inputs,
            sources: Default::default(),
            merge: Merge::default(),
            pointers: Pointer::new(mode.width, mode.height),
            poller,
            display,
            back,
            image,
            panel,
            kept,
            cursors,
            current_cursor: CursorStyle::Default,
            shapes,
            font,
            icons,
            wallpaper,
            desk,
            stack: Stack::default(),
            pending: Vec::new(),
            damage,
            cursor,
            prev_buttons: 0,
            grab: Grab::None,
            last_click_handle: None,
            last_click_at: now,
            clipboard: String::new(),
            launcher_open: false,
            apps,
            total_mem,
            max_windows,
            dead: Vec::new(),
            ready: Vec::new(),
            totals: FrameTotals::default(),
            cached_stats: SystemStats { used_mb: 0, total_mb: 0, cpu_pct: 0 },
            prev_busy_ticks: 0,
            prev_total_ticks: 0,
            last_taskbar_update: now,
            volume: None,
            volume_requests: Requests::default(),
            overlay: Overlay::default(),
            epoch: now,
            desktop,
        }
    }

    /// Drain everything pending, then put one frame on the panel.
    pub fn pass(&mut self) {
        let mut waited = false;
        let drain_until = Instant::now() + DRAIN_BUDGET;
        loop {
            // Zero and not one nanosecond: every turn after the first is
            // consuming what has already arrived, and a timeout of 1 ns is not
            // a spelling of "do not block" — it parks the thread on a deadline
            // that is already past, which is the whole of #156.
            let timeout = if waited { Duration::ZERO } else { FRAME_INTERVAL };
            if !self.drain(timeout) {
                break;
            }
            waited = true;
            // The clause that keeps one client from owning the loop: a peer
            // with something to send on every pass keeps its handle ready
            // forever, and a drain that only ended when nothing was ready would never
            // composite.
            if Instant::now() >= drain_until {
                break;
            }
        }
        self.tick_taskbar();
        if self.overlay.expire(self.overlay_now()) {
            self.damage.add(overlay_rect(&self.desk));
        }
        self.present();
        self.follow_display();
        self.prompt_gone();
    }

    /// One turn of the drain, or `false` when nothing was ready.
    fn drain(&mut self, timeout: Duration) -> bool {
        self.ready.clear();
        let mut ready = std::mem::take(&mut self.ready);
        self.poller.wait(1, timeout.as_nanos() as u64, |token| ready.push(token));
        self.ready = ready;

        let inputs_ready = self.is_ready(self.inputs.as_handle().raw());
        let sources_ready: Vec<usize> = (0..CONNECTIONS)
            .filter(|&n| self.sources[n].as_ref().is_some_and(|s| self.is_ready(s.conn.as_handle().raw())))
            .collect();
        let accept_ready = self.is_ready(self.acceptor.as_handle().raw());
        let prompt_ready = self.is_ready(self.prompts.as_handle().raw());
        let clipboard_ready = self.is_ready(self.clipboards.as_handle().raw());
        let settings_ready = self.is_ready(self.settings.as_handle().raw());
        let outputs_ready = self.is_ready(self.outputs.as_handle().raw());
        let volume_ready = self.volume.as_ref().is_some_and(|(conn, _)| self.is_ready(conn.as_handle().raw()));
        let display_ready = self.display.handle().is_some_and(|handle| self.is_ready(handle.raw()));
        let session_ready = self.desktop.conn().is_some_and(|c| self.is_ready(c.as_handle().raw()));
        let opened_ready = self.is_ready(self.desktop.wake().as_handle().raw());
        let client_ready = self.stack.iter().any(|w| self.is_ready(w.client.conn.as_handle().raw()))
            || self.pending.iter().any(|p| self.is_ready(p.conn.as_handle().raw()))
            || self.watcher.as_ref().is_some_and(|w| self.is_ready(w.conn.as_handle().raw()));

        // A handshake that never completes is the reason this deadline exists,
        // and the sweep has to happen on a pass that found nothing ready too —
        // otherwise a silent client is only ever timed out by some *other*
        // client's traffic.
        let now = Instant::now();
        for p in self.pending.iter().filter(|p| now.duration_since(p.since) >= HANDSHAKE_TIMEOUT) {
            let reason = match p.copy {
                Some(_) => DropReason::CopyTimeout,
                None => DropReason::HandshakeTimeout,
            };
            eprintln!("compositor: dropping client {} — {}", p.conn.as_handle().raw().0, reason);
        }
        self.pending.retain(|p| now.duration_since(p.since) < HANDSHAKE_TIMEOUT);
        if self.output.as_ref().is_some_and(|d| d.overdue()) {
            self.drop_output(format!("its hello did not come within {HANDSHAKE_TIMEOUT:?}"));
        }
        // After the sweep: a driver it dropped is not read.
        let output_ready = self.output.as_ref().is_some_and(|d| self.is_ready(d.conn().as_handle().raw()));

        if !inputs_ready
            && sources_ready.is_empty()
            && !accept_ready
            && !prompt_ready
            && !clipboard_ready
            && !settings_ready
            && !outputs_ready
            && !output_ready
            && !client_ready
            && !volume_ready
            && !display_ready
            && !session_ready
            && !opened_ready
        {
            return false;
        }

        self.dead.clear();
        if display_ready {
            self.display.woke();
        }
        if inputs_ready {
            self.admit();
        }
        for n in sources_ready {
            self.source(n);
        }
        if accept_ready {
            self.accept(Port::Compositor);
        }
        if prompt_ready {
            self.accept(Port::Prompt);
        }
        if clipboard_ready {
            self.accept(Port::Clipboard);
        }
        if settings_ready {
            self.accept(Port::Display);
        }
        if outputs_ready {
            self.accept_output();
        }
        if output_ready {
            let mode = self.display.mode();
            if let Some(driver) = self.output.as_mut() {
                match driver.read(mode) {
                    Ok(()) => self.poller.watch(driver.conn(), READABLE, driver.conn().as_handle().raw().0 as u64),
                    Err(why) => self.drop_output(why),
                }
            }
        }
        if volume_ready {
            self.volume_answers();
        }
        if session_ready {
            self.session_said();
        }
        if opened_ready {
            if self.desktop.opened() {
                if let Some(control) = self.desktop.conn() {
                    self.poller.watch(control, READABLE, control.as_handle().raw().0 as u64);
                }
            }
            self.poller.watch(self.desktop.wake(), READABLE, self.desktop.wake().as_handle().raw().0 as u64);
        }
        let frames = self.take_frames();
        self.dispatch(frames);
        self.watched();
        self.reap();
        self.prompt_gone();
        self.rearm(inputs_ready, accept_ready, prompt_ready, clipboard_ready, settings_ready);
        if outputs_ready {
            self.poller.watch(&self.outputs, READABLE, self.outputs.as_handle().raw().0 as u64);
        }
        true
    }

    /// A display driver off `display-output`, where none is connected.
    fn accept_output(&mut self) {
        match self.outputs.accept() {
            Err(e) => eprintln!("compositor: a display driver could not be accepted ({e:?})"),
            Ok(_) if self.output.is_some() => eprintln!("compositor: refusing a display driver — one is connected"),
            Ok(conn) => {
                self.poller.watch(&conn, READABLE, conn.as_handle().raw().0 as u64);
                self.output = Some(crate::output::Driver::new(conn, self.epoch));
            }
        }
    }

    fn drop_output(&mut self, why: String) {
        eprintln!("compositor: dropping the display driver — {why}");
        self.output = None;
    }

    /// Nanoseconds since the session's epoch, the clock the overlay is timed by.
    fn overlay_now(&self) -> u64 {
        self.epoch.elapsed().as_nanos() as u64
    }

    /// One request to soundserver's `volume` port, by a write that does not
    /// wait; connected first if it is not. A refusal ends the connection, by
    /// name, and what was pressed goes on the next one.
    fn send_volume(&mut self, request: Request) {
        if self.volume.is_none() {
            match endow::service("volume") {
                Ok(conn) => {
                    self.poller.watch(&conn, READABLE, conn.as_handle().raw().0 as u64);
                    self.volume = Some((conn, ipc::FrameRx::new()));
                }
                Err(e) => {
                    eprintln!("compositor: soundserver's volume port did not connect ({e:?})");
                    self.volume_requests.disconnected();
                    return;
                }
            }
        }
        let (conn, _) = self.volume.as_ref().expect("connected above");
        let sent = match request {
            Request::Step(delta) => conn.try_send(MSG_VOLUME_STEP, &VolumeStep { delta }),
            Request::Mute => conn.try_send_bytes(MSG_VOLUME_MUTE, &[]),
        };
        if let Err(e) = sent {
            eprintln!("compositor: a volume request was not taken ({e:?}); reconnecting on the next key");
            self.volume = None;
            self.volume_requests.disconnected();
        }
    }

    /// soundserver's answers, each shown on the overlay, and the next request
    /// sent behind each.
    fn volume_answers(&mut self) {
        loop {
            let Some((conn, rx)) = self.volume.as_mut() else { return };
            let shown = match rx.pump(conn) {
                RxStep::Idle => return,
                RxStep::Frame { msg_type: MSG_VOLUME, payload_len, .. } => {
                    match ipc::decode_payload::<Volume>(rx.payload(payload_len)) {
                        Ok(v) if v.muted != 0 => Shown::Muted,
                        Ok(v) => Shown::Level { position: v.position, bottom: v.bottom },
                        Err(e) => {
                            eprintln!("compositor: a volume answer did not decode ({e:?}); disconnecting");
                            self.volume = None;
                            self.volume_requests.disconnected();
                            return;
                        }
                    }
                }
                RxStep::Frame { msg_type: MSG_VOLUME_UNAVAILABLE, .. } => Shown::Unavailable,
                RxStep::Frame { msg_type, .. } => {
                    eprintln!("compositor: soundserver's volume port sent message {msg_type}; disconnecting");
                    self.volume = None;
                    self.volume_requests.disconnected();
                    return;
                }
                RxStep::Eof | RxStep::Malformed => {
                    eprintln!("compositor: soundserver's volume port ended; reconnecting on the next key");
                    self.volume = None;
                    self.volume_requests.disconnected();
                    return;
                }
            };
            self.overlay.answered(shown, self.overlay_now());
            self.damage.add(overlay_rect(&self.desk));
            if let Some(next) = self.volume_requests.answered() {
                self.send_volume(next);
            }
        }
    }

    fn is_ready(&self, handle: RawHandle) -> bool {
        self.ready.contains(&(handle.0 as u64))
    }

    fn pixel_format(&self) -> u32 {
        self.display.screen().pixel_format_raw()
    }

    /// `key` from `slot`'s device `source`, and whatever it changed of the
    /// merged keyboard.
    fn merged_key(&mut self, slot: Slot, source: u16, key: RawKeyEvent) -> Result<(), Full> {
        let delivered: Vec<RawKeyEvent> = self.merge.key(slot, source, key)?.collect();
        for event in delivered {
            self.key(event);
        }
        Ok(())
    }

    /// `slot`'s device `source` went: what it held is released.
    fn gone(&mut self, slot: Slot, source: u16) -> State {
        let released: Vec<RawKeyEvent> = self.merge.gone(slot, source).collect();
        released.into_iter().for_each(|key| self.key(key));
        self.pointers.gone(slot, source)
    }

    /// One transition of the merged keyboard.
    fn key(&mut self, raw: RawKeyEvent) {
        if let Some(asked) = sign_out_key(raw, self.desktop.waiting.is_some(), self.prompt.is_some()) {
            match asked {
                SignOutKey::Ask => self.desktop.sign_out(false),
                SignOutKey::Force => self.desktop.sign_out(true),
                SignOutKey::Cancel => self.desktop.cancel(),
            }
            self.damage.add(sign_out_rect(&self.desk));
            return;
        }
        let event = window::KeyEvent::from(raw);
        let focused = self.stack.focused();
        let action = key_action(
            event.into(),
            focused.map(|i| self.stack[i].mode),
            self.launcher_open,
            self.prompt.is_some(),
        );
        match action {
            KeyAction::Ignore => {}
            KeyAction::Forward => {
                // While a prompt is up it is the focused window, and is
                // given what its gate lets through and nothing else is.
                let given = match &mut self.prompt {
                    Some((_, gate)) => gate.key(event.into()),
                    None => !self.owed.owed(event.into()),
                };
                if !given {
                    return;
                }
                if let Some(i) = focused {
                    deliver(&mut self.dead, &self.stack[i], window::MSG_KEY_INPUT, &event);
                }
            }
            KeyAction::CloseLauncher => {
                self.launcher_open = false;
                self.damage.add(self.launcher_rect());
            }
            KeyAction::CycleFocus => {
                if self.stack.cycle() {
                    self.damage_all();
                }
            }
            KeyAction::SpawnTerminal => self.launch("/system/bin/terminal"),
            KeyAction::Volume(key) => {
                if let Some(request) = self.volume_requests.press(key) {
                    self.send_volume(request);
                }
            }
            KeyAction::Paste => {
                if let Some(i) = focused {
                    self.paste(i);
                }
            }
            // The three that move a window. Each damages the rect it is
            // vacating first, because that is only knowable before it
            // moves.
            KeyAction::SetMode(mode) => {
                let Some(idx) = focused else { return };
                self.damage.add(self.stack[idx].frame(&self.desk.chrome));
                self.retarget(idx, mode);
                self.damage_all();
            }
            KeyAction::Minimize => {
                let Some(idx) = focused else { return };
                self.damage.add(self.stack[idx].frame(&self.desk.chrome));
                self.stack[idx].minimized = true;
                self.damage_all();
            }
            KeyAction::CloseFocused => {
                let Some(idx) = focused else { return };
                self.damage.add(self.stack[idx].frame(&self.desk.chrome));
                let win = self.stack.remove(idx);
                note_closed("GUI+Q", &win, self.stack.len());
                let _ = win.client.conn.try_signal(window::MSG_WINDOW_CLOSE);
                self.damage_all();
            }
        }
    }

    /// What the merged pointer did over `states`.
    fn motion(&mut self, states: &[State]) {
        let sample = fold_mouse(self.prev_buttons, states);
        if sample.reports == 0 {
            return;
        }

        let (was, was_style) = (self.cursor, self.current_cursor);
        self.cursor = cursor_from_abs(sample.abs_x, sample.abs_y, self.desk.screen);
        let delta = Point { x: self.cursor.x - was.x, y: self.cursor.y - was.y };

        let wanted =
            cursor_style(&self.desk, &self.stack, &self.grab, self.cursor, self.launcher_open);
        if self.hw_cursor() {
            if wanted != self.current_cursor {
                self.current_cursor = wanted;
                self.show_cursor();
            } else {
                self.display.move_cursor((self.cursor.x as u32, self.cursor.y as u32));
            }
        } else {
            self.current_cursor = wanted;
            self.damage.add(cursor::drawn(was_style, was, self.desk.scale));
            self.damage.add(cursor::drawn(wanted, self.cursor, self.desk.scale));
        }

        if sample.pressed {
            self.press(sample.buttons);
        }
        if sample.released {
            self.release(sample.buttons);
        }
        if sample.left_held() {
            self.hold(sample.buttons, delta);
        }
        if sample.scroll != 0 {
            if let Hit::Content(idx) =
                hit_test(&self.desk, &self.stack, self.cursor, self.launcher_open)
            {
                let ev = mouse_event(
                    &self.stack[idx],
                    self.cursor,
                    sample.buttons,
                    window::MOUSE_SCROLL,
                    0,
                    sample.scroll.clamp(-128, 127) as i8,
                );
                deliver(&mut self.dead, &self.stack[idx], window::MSG_MOUSE_INPUT, &ev);
            }
        }
        self.prev_buttons = sample.buttons;
    }

    fn press(&mut self, buttons: u8) {
        let at = self.cursor;
        match hit_test(&self.desk, &self.stack, at, self.launcher_open) {
            Hit::CloseButton(idx) => {
                let win = self.stack.remove(idx);
                note_closed("its close button", &win, self.stack.len());
                self.damage.add(win.frame(&self.desk.chrome));
                let _ = win.client.conn.try_signal(window::MSG_WINDOW_CLOSE);
                self.damage_all();
            }
            Hit::MinimizeButton(idx) => {
                self.stack[idx].minimized = true;
                self.damage_all();
            }
            Hit::MaximizeButton(idx) => {
                let i = self.stack.raise(idx);
                self.damage.add(self.stack[i].frame(&self.desk.chrome));
                self.retarget(i, toggled(self.stack[i].mode));
                self.damage_all();
            }
            Hit::TitleBar(idx) => {
                let i = self.stack.raise(idx);
                self.damage.add(self.stack[i].frame(&self.desk.chrome));

                let now = Instant::now();
                let handle = self.stack[i].client.conn.as_handle().raw();
                let double = Some(handle) == self.last_click_handle
                    && now.duration_since(self.last_click_at) < DOUBLE_CLICK_TIME;
                if double {
                    self.retarget(i, toggled(self.stack[i].mode));
                    self.last_click_handle = None;
                    self.last_click_at = now - DOUBLE_CLICK_TIME;
                } else {
                    self.last_click_handle = Some(handle);
                    self.last_click_at = now;
                    self.grab = Grab::on_title(self.stack[i].id, self.stack[i].mode, at);
                }
                self.damage_all();
            }
            Hit::ResizeCorner(idx) => {
                let i = self.stack.raise(idx);
                self.grab = Grab::Resizing { window: self.stack[i].id };
                self.damage_all();
            }
            Hit::Content(idx) => {
                // The one content a press reaches while a prompt is up is the
                // prompt's, and only a press its gate lets through.
                if let Some((_, gate)) = &mut self.prompt {
                    if !gate.press() {
                        return;
                    }
                }
                if self.launcher_open {
                    self.launcher_open = false;
                    self.damage.add(self.launcher_rect());
                }
                let i = self.stack.raise(idx);
                if i != idx {
                    self.damage_all();
                }
                let ev =
                    mouse_event(&self.stack[i], at, buttons, window::MOUSE_PRESS, 1, 0);
                deliver(&mut self.dead, &self.stack[i], window::MSG_MOUSE_INPUT, &ev);
            }
            Hit::TaskbarItem(idx) => {
                match tab_action(&self.stack, idx) {
                    TabAction::Reveal => {
                        self.stack[idx].minimized = false;
                        self.stack.raise(idx);
                    }
                    TabAction::Minimize => self.stack[idx].minimized = true,
                    TabAction::Raise => {
                        self.stack.raise(idx);
                    }
                }
                self.damage_all();
            }
            Hit::TaskbarNew => {
                // Both rectangles, because reading `/apps` may change the
                // popup's height between them.
                self.damage.add(self.launcher_rect());
                if !self.launcher_open {
                    self.apps = launcher_apps();
                    self.desk.apps = self.apps.len();
                }
                self.launcher_open = !self.launcher_open;
                self.damage.add(self.launcher_rect());
                self.damage.add(self.desk.taskbar(self.stack.len()).strip());
            }
            Hit::LauncherItem(idx) => {
                if let Some((_, program)) = self.apps.get(idx) {
                    let program = program.clone();
                    self.launch(&program);
                }
                self.launcher_open = false;
                self.damage.add(self.launcher_rect());
            }
            Hit::Blocked => {}
            Hit::Desktop => {
                if self.launcher_open {
                    self.launcher_open = false;
                    self.damage.add(self.launcher_rect());
                }
            }
        }
    }

    fn release(&mut self, buttons: u8) {
        let given = match &mut self.prompt {
            Some((_, gate)) => gate.release(),
            None => !self.owed.release(),
        };
        if let (Some(i), true) = (self.stack.focused(), given) {
            let ev =
                mouse_event(&self.stack[i], self.cursor, buttons, window::MOUSE_RELEASE, 1, 0);
            deliver(&mut self.dead, &self.stack[i], window::MSG_MOUSE_INPUT, &ev);
        }
        match self.grab.release(&self.desk, &self.stack, self.cursor) {
            Released::Nothing => {}
            Released::Snapped { window: idx, mode } => {
                self.damage.add(self.stack[idx].frame(&self.desk.chrome));
                self.retarget(idx, mode);
                self.damage_all();
            }
            Released::Resized { window: idx } => {
                let pf = self.pixel_format();
                settle(&mut self.stack[idx], pf, self.desk.scale.wire(), &mut self.dead);
                self.damage_all();
            }
        }
    }

    fn hold(&mut self, buttons: u8, delta: Point) {
        match self.grab.hold(&self.desk, &mut self.stack, self.cursor, delta) {
            Held::Idle => {}
            Held::Free => {
                if let Some(i) = self.stack.focused() {
                    let ev =
                        mouse_event(&self.stack[i], self.cursor, buttons, window::MOUSE_MOVE, 0, 0);
                    deliver(&mut self.dead, &self.stack[i], window::MSG_MOUSE_INPUT, &ev);
                }
            }
            Held::Restored { window: idx } => {
                let pf = self.pixel_format();
                settle(&mut self.stack[idx], pf, self.desk.scale.wire(), &mut self.dead);
                self.damage.add(self.desk.screen);
            }
            Held::Moved { from, to, .. } => {
                self.damage.add(from);
                self.damage.add(to);
            }
        }
    }

    /// A source's connection off `input`, in the first free slot, or refused
    /// by name when every slot is taken.
    fn admit(&mut self) {
        match self.inputs.accept() {
            Err(e) => eprintln!("compositor: an input source could not be accepted ({e:?})"),
            Ok(conn) => match self.sources.iter().position(Option::is_none) {
                None => eprintln!("compositor: refusing an input source — {CONNECTIONS} are connected"),
                Some(n) => {
                    self.poller.watch(&conn, READABLE, conn.as_handle().raw().0 as u64);
                    self.sources[n] = Some(Source { conn, rx: FrameRx::new() });
                }
            },
        }
    }

    /// What source `n` sent, each frame whole; one that does not decode, or
    /// ends, drops it, and everything it held is released.
    fn source(&mut self, n: usize) {
        let slot = Slot::connection(n).expect("a source's index is a slot");
        let mut states = Vec::new();
        let mut ended = None;
        for _ in 0..SOURCE_FRAMES {
            let source = self.sources[n].as_mut().expect("a ready source is held");
            let record = match source.rx.pump(&source.conn) {
                RxStep::Idle => break,
                RxStep::Eof => {
                    ended = Some("its connection ended".to_string());
                    break;
                }
                RxStep::Malformed => {
                    ended = Some("a frame no protocol here can make".to_string());
                    break;
                }
                RxStep::Frame { msg_type, payload_len, .. } => wire::decode(msg_type, source.rx.payload(payload_len)),
            };
            match record {
                Ok(Record::Key { source, key }) => {
                    if let Err(full) = self.merged_key(slot, source, key) {
                        ended = Some(format!("{full:?}: more than {DEVICES} of its devices hold something"));
                        break;
                    }
                }
                Ok(Record::Pointer(report)) => match self.pointers.report(slot, report) {
                    Ok(state) => states.push(state),
                    Err(full) => {
                        ended = Some(format!("{full:?}: more than {DEVICES} of its devices hold something"));
                        break;
                    }
                },
                Ok(Record::Gone { source }) => states.push(self.gone(slot, source)),
                Err(why) => {
                    ended = Some(format!("a record refused: {why:?}"));
                    break;
                }
            }
        }
        match ended {
            None => {
                let conn = &self.sources[n].as_ref().expect("a ready source is held").conn;
                self.poller.watch(conn, READABLE, conn.as_handle().raw().0 as u64);
            }
            Some(why) => {
                eprintln!("compositor: dropping input source {n} — {why}");
                self.sources[n] = None;
                let released: Vec<RawKeyEvent> = self.merge.end(slot).collect();
                released.into_iter().for_each(|key| self.key(key));
                states.push(self.pointers.end(slot));
            }
        }
        self.motion(&states);
    }

    /// One connection off `port`.
    fn accept(&mut self, port: Port) {
        // `accept` installs a descriptor, so it answers `ResourceExhausted` on
        // a full handle table — and clients drive that table, one handle per
        // connection. The connection is lost either way; the desktop is not.
        let acceptor = match port {
            Port::Compositor => &self.acceptor,
            Port::Prompt => &self.prompts,
            Port::Clipboard => &self.clipboards,
            Port::Display => &self.settings,
        };
        match acceptor.accept() {
            Err(e) => eprintln!("compositor: a connection could not be accepted ({e:?})"),
            Ok(_) if port == Port::Clipboard && self.watcher.is_some() => {
                eprintln!("compositor: refusing a clipboard watcher's connection — one is watching");
            }
            Ok(conn)
                if !toyos_desktop::pending_admits(
                    port,
                    self.pending.iter().map(|p| p.port),
                    MAX_PENDING_CONNS as usize,
                ) =>
            {
                match port {
                    Port::Prompt => eprintln!("compositor: refusing a prompt's connection — another is waiting"),
                    Port::Clipboard => {
                        eprintln!("compositor: refusing a clipboard watcher's connection — another is waiting")
                    }
                    Port::Display => eprintln!("compositor: refusing a display request — another is waiting"),
                    Port::Compositor => eprintln!(
                        "compositor: refusing client {} — {MAX_PENDING_CONNS} connections are already \
                         waiting to say what they want",
                        conn.as_handle().raw().0
                    ),
                }
            }
            Ok(conn) => {
                self.poller.watch(&conn, READABLE, conn.as_handle().raw().0 as u64);
                self.pending.push(PendingConn {
                    conn,
                    rx: ClientRx::new(),
                    since: Instant::now(),
                    copy: None,
                    port,
                });
            }
        }
    }

    /// Every whole frame that arrived, off the connections and in memory.
    ///
    /// Collected before anything acts on one: the read side is finished by the
    /// time a message is dispatched, so no branch of [`dispatch`](Self::dispatch)
    /// can park on a peer.
    fn take_frames(&mut self) -> Vec<ClientFrame> {
        let mut out: Vec<ClientFrame> = Vec::new();

        for i in 0..self.pending.len() {
            if !self.is_ready(self.pending[i].conn.as_handle().raw()) {
                continue;
            }
            let step = {
                let p = &mut self.pending[i];
                p.rx.pump(&p.conn)
            };
            let handle = self.pending[i].conn.as_handle();
            match step {
                RxStep::Idle => {}
                RxStep::Eof => mark_dead(&mut self.dead, handle.raw(), DropReason::Gone),
                RxStep::Malformed => {
                    mark_dead(&mut self.dead, handle.raw(), DropReason::OutOfProtocol)
                }
                RxStep::Frame { msg_type, payload_len, .. } => {
                    let mut frame = ClientFrame::new(handle.raw(), msg_type);
                    frame.set_payload(self.pending[i].rx.payload(payload_len));
                    // A connection is identified by its first frame and by
                    // nothing else. `MSG_CREATE_WINDOW` promotes it to a
                    // window; `MSG_COPY_BEGIN` puts it back to wait for its
                    // commit; anything else is a one-shot request, answered and
                    // closed.
                    //
                    // One promotion per pass keeps `i` meaningful across the
                    // `remove`; the rest are re-armed below and served next
                    // pass.
                    let p = self.pending.remove(i);
                    frame.conn = Some(p.conn);
                    frame.copy = p.copy;
                    frame.port = p.port;
                    out.push(frame);
                    break;
                }
            }
        }

        for i in 0..self.stack.len() {
            if !self.is_ready(self.stack[i].client.conn.as_handle().raw()) {
                continue;
            }
            let win = &mut self.stack[i];
            let step = win.client.rx.pump(&win.client.conn);
            let handle = win.client.conn.as_handle();
            match step {
                RxStep::Idle => {}
                RxStep::Eof => mark_dead(&mut self.dead, handle.raw(), DropReason::Gone),
                RxStep::Malformed => {
                    mark_dead(&mut self.dead, handle.raw(), DropReason::OutOfProtocol)
                }
                RxStep::Frame { msg_type, payload_len, .. } => {
                    let mut frame = ClientFrame::new(handle.raw(), msg_type);
                    frame.set_payload(win.client.rx.payload(payload_len));
                    out.push(frame);
                }
            }
        }
        out
    }

    fn dispatch(&mut self, frames: Vec<ClientFrame>) {
        for mut frame in frames {
            let handle = frame.handle;
            // A connection holding a region may send its commit and nothing
            // else, and a commit is only ever of a region held.
            match (frame.copy.take(), frame.msg_type) {
                (Some(region), window::MSG_COPY_COMMIT) => {
                    self.copy_commit(frame, region);
                    continue;
                }
                (Some(_), _) | (None, window::MSG_COPY_COMMIT) => {
                    mark_dead(&mut self.dead, handle, DropReason::OutOfProtocol);
                    continue;
                }
                (None, _) => {}
            }
            match (frame.port, frame.msg_type) {
                // A connection on the prompt port is for its window and nothing else.
                (Port::Prompt, window::MSG_CREATE_WINDOW) | (Port::Compositor, _) => {}
                (Port::Clipboard, window::MSG_CLIPBOARD_WATCH) if frame.payload().is_empty() => {
                    self.watch_begins(frame);
                    continue;
                }
                (Port::Prompt | Port::Clipboard, _) => {
                    mark_dead(&mut self.dead, handle, DropReason::OutOfProtocol);
                    continue;
                }
                (Port::Display, _) => {
                    self.display_request(frame);
                    continue;
                }
            }
            match frame.msg_type {
                window::MSG_CREATE_WINDOW => self.create_window(frame),
                window::MSG_COPY_BEGIN => self.copy_begin(frame),
                window::MSG_PRESENT => {
                    let Ok(rect) = ipc::decode_payload::<window::PresentRect>(frame.payload()) else {
                        mark_dead(&mut self.dead, handle, DropReason::OutOfProtocol);
                        continue;
                    };
                    if let Some(i) = self.stack.find(|w| w.client.conn.as_handle().raw() == handle) {
                        let pf = self.pixel_format();
                        let win = &mut self.stack[i];
                        win.presented = true;
                        win.client.presents += 1;
                        match win.client.buffers.presented(rect.generation) {
                            Presented::Shown => {}
                            // Drawn into the buffer a resize gave: shown whole
                            // from here, and the buffer the window was owed
                            // meanwhile given now.
                            Presented::Swapped { owed, .. } => {
                                let shown = win.client.buffers.shown();
                                (win.buf_w, win.buf_h) = (shown.width, shown.height);
                                self.damage.add(win.frame(&self.desk.chrome));
                                let wanted = (win.content.w(), win.content.h(), self.desk.scale.wire());
                                if owed && wanted != (shown.width, shown.height, shown.scale) {
                                    settle(win, pf, self.desk.scale.wire(), &mut self.dead);
                                }
                            }
                            Presented::Unknown => {
                                eprintln!(
                                    "compositor: client {} presented generation {}, a buffer it was never given",
                                    handle.0, rect.generation
                                );
                                mark_dead(&mut self.dead, handle, DropReason::OutOfProtocol);
                                continue;
                            }
                        }
                        let claim = Rect::from_wire(rect.x, rect.y, rect.w, rect.h);
                        self.damage.add(self.stack[i].present_damage(claim));
                    }
                }
                window::MSG_DESTROY_WINDOW => {
                    if let Some(i) = self.stack.find(|w| w.client.conn.as_handle().raw() == handle) {
                        let gone = self.stack.remove(i);
                        note_closed("the client itself", &gone, self.stack.len());
                        self.damage.add(gone.frame(&self.desk.chrome));
                        self.damage_all();
                    }
                }
                window::MSG_CLIPBOARD_SET => self.client_copy(handle, frame.payload().to_vec()),
                window::MSG_RETIRED_CLIPBOARD_SET_SHM => {
                    mark_dead(&mut self.dead, handle, DropReason::Retired);
                }
                window::MSG_LAYOUT_CHANGED => {
                    // The compositor is the root of the surface tree and
                    // translates nothing, so it has no layout of its own to
                    // update — it exists here only so that every window gets
                    // the same answer to a question one of them changed.
                    // Delivered to the sender too: the config is the layout,
                    // and re-reading a file one has just written is cheaper
                    // than a rule about who is exempt.
                    for win in self.stack.iter() {
                        deliver_signal(&mut self.dead, win, window::MSG_LAYOUT_CHANGED);
                    }
                }
                window::MSG_SET_CURSOR => {
                    let Ok(style) = ipc::decode_payload::<u32>(frame.payload()) else {
                        mark_dead(&mut self.dead, handle, DropReason::OutOfProtocol);
                        continue;
                    };
                    if let Some(i) = self.stack.find(|w| w.client.conn.as_handle().raw() == handle) {
                        self.stack[i].cursor_style = cursor_from_wire(style);
                    }
                }
                // A connection's first frame, bare, answered and closed with
                // `frame.conn`; one with a payload, or on a window's
                // connection, is not this protocol.
                toyos_inspect::MSG_INSPECT => match &frame.conn {
                    Some(conn) if frame.payload().is_empty() => self.answer_inspect(conn),
                    _ => mark_dead(&mut self.dead, handle, DropReason::OutOfProtocol),
                },
                _ => {}
            }
        }
    }

    fn create_window(&mut self, frame: ClientFrame) {
        let handle = frame.handle;
        // `frame.conn` is dropped by every early return here, which closes the
        // handle: there is no window to remove yet.
        let Ok(req) = ipc::decode_payload::<window::CreateWindowRequest>(frame.payload()) else {
            return;
        };
        // A window *is* a connection its first frame promoted, so a second
        // `MSG_CREATE_WINDOW` comes with nothing to promote. `conn` is `None`
        // for every frame off an established window, and reading that as a bug
        // rather than as a protocol error made one message from any client
        // fatal.
        let Some(conn) = frame.conn else {
            mark_dead(&mut self.dead, handle, DropReason::OutOfProtocol);
            return;
        };

        let level = match (frame.port, req.flags & window::WINDOW_FLAG_TOPMOST != 0) {
            (Port::Prompt, _) => Level::Prompt,
            (_, true) => Level::Topmost,
            (_, false) => Level::Ordinary,
        };
        // One prompt at a time: the prompt layer is full.
        if !self.stack.admits(level) {
            eprintln!("compositor: refusing a prompt from client {} — one is already up", handle.0);
            let reason = window::REFUSED_AT_CAPACITY;
            let _ = conn.try_send(window::MSG_WINDOW_REFUSED, &window::WindowRefused { reason });
            return;
        }

        // A size in logical pixels is the output's scale's to convert, and a
        // toolkit's preference to fit to the work area.
        let size = match toyos_desktop::requested(window::Size::of(&req), &self.desk) {
            Requested::Fitted { asked, given } => {
                println!(
                    "compositor: client {} asked for {}x{} logical, {}x{} at scale {}; given {}x{}, what the work area holds",
                    handle.0, req.width, req.height, asked.0, asked.1, self.desk.scale, given.0, given.1
                );
                given
            }
            requested => requested.size(),
        };

        // Every refusal below is an answer to untrusted input, so none of them
        // is a panic and none is a silent shrink of what was asked for.
        let refusal = match toyos_desktop::create_verdict(
            size,
            self.desk.screen,
            level,
            self.stack.clients(),
            self.max_windows,
        ) {
            Verdict::Allow => None,
            Verdict::AtCapacity => Some(window::REFUSED_AT_CAPACITY),
            Verdict::TooLarge => Some(window::REFUSED_TOO_LARGE),
        };
        if let Some(reason) = refusal {
            eprintln!(
                "compositor: refusing {}x{} window from client {} ({} live, max {}), reason \
                 {reason}",
                size.0,
                size.1,
                handle.0,
                self.stack.clients(),
                self.max_windows
            );
            let _ =
                conn.try_send(window::MSG_WINDOW_REFUSED, &window::WindowRefused { reason });
            return;
        }

        let content = self.desk.chrome.initial_content(
            Some((size.0 as i32, size.1 as i32)),
            self.stack.len(),
            self.desk.screen,
        );
        // The client gets the one writable handle and draws into the buffer
        // with plain stores; the compositor reads it through a read-only
        // mapping. Each holds the region apart, so a client that exits leaves
        // the compositor's buffer intact until the window goes.
        let (shm, client_shm) = match ReadOnly::create((content.area() * 4) as usize) {
            Ok(made) => made,
            Err(e) => {
                eprintln!(
                    "compositor: refusing {}x{} window from client {} — there is no memory \
                     for it ({e:?})",
                    content.w(),
                    content.h(),
                    handle.0
                );
                let _ = conn.try_send(
                    window::MSG_WINDOW_REFUSED,
                    &window::WindowRefused { reason: window::REFUSED_NO_MEMORY },
                );
                return;
            }
        };
        let title = if req.title_len > 0 {
            let len = (req.title_len as usize).min(30);
            String::from_utf8_lossy(&req.title[..len]).into_owned()
        } else {
            String::new()
        };
        let buffer = Buffer { shm, width: content.w(), height: content.h(), scale: self.desk.scale.wire() };
        let at = self.stack.insert(Window::new(
            Client { conn, buffers: Buffers::new(buffer), rx: ClientRx::new(), presents: 0, frames: 0 },
            content,
            title,
            level,
            CursorStyle::Default,
        ));
        note_opened(handle, content, self.stack.len());
        if level == Level::Prompt {
            // Whatever the pointer was doing ends here, and the launcher
            // closes: nothing under the prompt moves on input it is given.
            self.prompt = Some((self.stack[at].id, PromptGate::default()));
            self.grab = Grab::None;
            if self.launcher_open {
                self.launcher_open = false;
                self.damage.add(self.launcher_rect());
            }
        }

        self.poller.watch(&self.stack[at].client.conn, READABLE, handle.0 as u64);
        let pixel_format = self.pixel_format();
        deliver_with_handle(
            &mut self.dead,
            &self.stack[at],
            client_shm,
            window::MSG_WINDOW_CREATED,
            &window::WindowInfo {
                width: content.w() as u32,
                height: content.h() as u32,
                stride: content.w() as u32,
                pixel_format,
                scale: self.desk.scale.wire().wire(),
                generation: 0,
            },
        );
        self.damage_all();
    }

    /// `MSG_COPY_BEGIN`: make the region the client's text goes into, send it,
    /// and hold the connection for its commit.
    fn copy_begin(&mut self, frame: ClientFrame) {
        let handle = frame.handle;
        let info = ipc::decode_payload::<window::ClipboardShmMsg>(frame.payload());
        let (Ok(info), Some(conn)) = (info, frame.conn) else {
            mark_dead(&mut self.dead, handle, DropReason::OutOfProtocol);
            return;
        };
        let len = info.len as usize;
        if !window::copy_fits(len) {
            eprintln!(
                "compositor: refusing a copy of {len} bytes from client {}, max {}",
                handle.0,
                window::MAX_CLIPBOARD_BYTES
            );
            return;
        }
        let (region, theirs) = match ReadOnly::create(len) {
            Ok(made) => made,
            Err(e) => {
                eprintln!(
                    "compositor: client {} gets no copy — no region of {len} bytes for it ({e:?})",
                    handle.0
                );
                return;
            }
        };
        if let Err(e) = conn.try_signal_with(window::MSG_COPY_REGION, [theirs]) {
            mark_dead(&mut self.dead, handle, e.into());
            return;
        }
        self.pending.push(PendingConn {
            conn,
            rx: ClientRx::new(),
            since: Instant::now(),
            copy: Some(CopyRegion::new(region)),
            port: Port::Compositor,
        });
    }

    /// `MSG_COPY_COMMIT` on a connection holding `region`: take the text out
    /// once, and let the connection close.
    fn copy_commit(&mut self, frame: ClientFrame, region: CopyRegion) {
        if !frame.payload().is_empty() {
            mark_dead(&mut self.dead, frame.handle, DropReason::OutOfProtocol);
            return;
        }
        self.client_copy(frame.handle, region.take());
    }

    /// A client's copy: the clipboard, and the watcher's next delivery.
    fn client_copy(&mut self, from: RawHandle, bytes: Vec<u8>) {
        let Some(text) = text(from, bytes) else { return };
        self.clipboard = text;
        if self.watch.client_set() {
            self.deliver_clipboard();
        }
    }

    /// `MSG_CLIPBOARD_WATCH` as the first frame off the clipboard port: the
    /// connection is the watcher, and has asked.
    fn watch_begins(&mut self, frame: ClientFrame) {
        let Some(conn) = frame.conn else {
            mark_dead(&mut self.dead, frame.handle, DropReason::OutOfProtocol);
            return;
        };
        self.poller.watch(&conn, READABLE, frame.handle.0 as u64);
        println!("compositor: the clipboard is watched by client {}", frame.handle.0);
        self.watcher = Some(Watcher { conn, rx: ClientRx::new() });
        self.watcher_asks();
    }

    /// One frame off the watcher's connection, if it sent a whole one.
    fn watched(&mut self) {
        let Some(w) = self.watcher.as_mut() else { return };
        let handle = w.conn.as_handle().raw();
        if !self.ready.contains(&(handle.0 as u64)) {
            return;
        }
        let (msg_type, len) = match w.rx.pump(&w.conn) {
            RxStep::Idle => return,
            RxStep::Eof => return mark_dead(&mut self.dead, handle, DropReason::Gone),
            RxStep::Malformed => return mark_dead(&mut self.dead, handle, DropReason::OutOfProtocol),
            RxStep::Frame { msg_type, payload_len, .. } => (msg_type, payload_len),
        };
        let payload = w.rx.payload(len).to_vec();
        match msg_type {
            window::MSG_CLIPBOARD_WATCH if payload.is_empty() => self.watcher_asks(),
            window::MSG_CLIPBOARD_SET => {
                let landing = self.watch.set();
                self.watcher_sets(handle, landing, payload);
            }
            window::MSG_COPY_BEGIN => self.watcher_begins(handle, &payload),
            window::MSG_COPY_COMMIT if payload.is_empty() => {
                let Some((region, landing)) = self.watch.commit() else {
                    return mark_dead(&mut self.dead, handle, DropReason::OutOfProtocol);
                };
                self.watcher_sets(handle, landing, region.take());
            }
            _ => mark_dead(&mut self.dead, handle, DropReason::OutOfProtocol),
        }
    }

    fn watcher_asks(&mut self) {
        if self.watch.ask() {
            self.deliver_clipboard();
        }
    }

    /// The watcher's set: it lands only where `toyos_desktop::clipboard` says,
    /// and is answered whether or not it did, so the watcher has one in flight.
    fn watcher_sets(&mut self, from: RawHandle, landing: Landing, bytes: Vec<u8>) {
        match (landing, text(from, bytes)) {
            (_, None) => {}
            (Landing::Lands, Some(text)) => self.clipboard = text,
            (Landing::Stale, Some(text)) => eprintln!(
                "compositor: refusing a clipboard of {} bytes from the watcher, client {} — it was \
                 made before the watcher saw the copy now in its pipe",
                text.len(),
                from.0
            ),
        }
        let w = self.watcher.as_ref().expect("a set is read only off the watcher");
        if let Err(e) = w.conn.try_signal(window::MSG_CLIPBOARD_SET_ANSWERED) {
            mark_dead(&mut self.dead, from, e.into());
        }
    }

    /// `MSG_COPY_BEGIN` from the watcher: the region its set goes into, held
    /// until its commit.
    fn watcher_begins(&mut self, from: RawHandle, payload: &[u8]) {
        let Ok(info) = ipc::decode_payload::<window::ClipboardShmMsg>(payload) else {
            return mark_dead(&mut self.dead, from, DropReason::OutOfProtocol);
        };
        let len = info.len as usize;
        if !window::copy_fits(len) {
            return mark_dead(&mut self.dead, from, DropReason::OutOfProtocol);
        }
        let (region, theirs) = match ReadOnly::create(len) {
            Ok(made) => made,
            Err(e) => {
                eprintln!("compositor: the watcher gets no region — no memory for {len} bytes ({e:?})");
                return mark_dead(&mut self.dead, from, DropReason::Refused(e));
            }
        };
        if self.watch.begin(CopyRegion::new(region)).is_err() {
            return mark_dead(&mut self.dead, from, DropReason::OutOfProtocol);
        }
        let w = self.watcher.as_ref().expect("a region is opened only off the watcher");
        if let Err(e) = w.conn.try_signal_with(window::MSG_COPY_REGION, [theirs]) {
            mark_dead(&mut self.dead, from, e.into());
        }
    }

    /// Send the watcher the clipboard, in the paste's two encodings, empty
    /// included. A refused send drops it by name.
    fn deliver_clipboard(&mut self) {
        let w = self.watcher.as_ref().expect("an ask is outstanding only while a watcher watches");
        let handle = w.conn.as_handle();
        let len = self.clipboard.len();
        let sent = if len <= window::MAX_INLINE_PAYLOAD {
            w.conn.try_send_bytes(window::MSG_CLIPBOARD_PASTE, self.clipboard.as_bytes()).map_err(DropReason::from)
        } else {
            match SharedMemory::create(len).and_then(|r| {
                r.bytes().write(0, self.clipboard.as_bytes());
                r.share_read_only()
            }) {
                Ok(theirs) => w
                    .conn
                    .try_send_with(window::MSG_CLIPBOARD_PASTE_SHM, &window::ClipboardShmMsg { len: len as u32 }, [theirs])
                    .map_err(DropReason::from),
                Err(e) => Err(DropReason::Refused(e)),
            }
        };
        if let Err(reason) = sent {
            mark_dead(&mut self.dead, handle.raw(), reason);
        }
    }

    /// One request off the display port, answered and its connection
    /// closed. One the port cannot carry is not answered: the connection is
    /// dropped by name.
    fn display_request(&mut self, frame: ClientFrame) {
        let handle = frame.handle;
        let (msg_type, len) = (frame.msg_type, frame.payload().len());
        let request = display::Request::decode(msg_type, frame.payload());
        let (Some(request), Some(conn)) = (request, frame.conn) else {
            eprintln!(
                "compositor: refusing a display request, message {msg_type} of {len} bytes, that the port does not carry"
            );
            mark_dead(&mut self.dead, handle, DropReason::OutOfProtocol);
            return;
        };
        let done = match request {
            display::Request::Get => Ok(()),
            display::Request::SetMode { width, height } => self.set_mode(width, height),
            display::Request::SetScale(setting) => self.set_scale(setting),
        };
        let sent = match done {
            Ok(()) => conn.try_send(display::MSG_STATE, &self.shown().wire()),
            Err(why) => {
                println!("compositor: a display request is refused: {why}");
                conn.try_send(display::MSG_REFUSED, &display::Refusal { reason: why.wire() })
            }
        };
        if let Err(e) = sent {
            mark_dead(&mut self.dead, handle, e.into());
        }
    }

    /// What the display shows, as its port answers it.
    fn shown(&self) -> display::Shown {
        display::Shown {
            width: self.display.mode().width,
            height: self.display.mode().height,
            scale: self.desk.scale.wire(),
            setting: self.kept.map_or(Setting::Auto, |k| Setting::Kept(k.wire())),
        }
    }

    /// `inspect`'s answer: the panel, how many windows there are and may be,
    /// and every frame composited so far. Counts only, never a title — every
    /// client holding `compositor` can ask.
    fn answer_inspect(&mut self, conn: &Connection) {
        let mut snap = toyos_inspect::Snapshot::new(toyos_inspect::DISPLAY);
        let mode = self.display.mode();
        snap.put("width", mode.width);
        snap.put("height", mode.height);
        snap.put("scale", self.desk.scale.to_string());
        snap.put("cursor", if self.hw_cursor() { "hardware" } else { "software" });
        snap.put("windows.open", self.stack.len());
        snap.put("windows.max", self.max_windows);
        self.totals.inspect(&mut snap);
        let encoded =
            snap.encode().unwrap_or_else(|why| panic!("compositor: its snapshot: {why}"));
        if let Err(e) = conn.try_send_bytes(toyos_inspect::MSG_SNAPSHOT, &encoded) {
            mark_dead(&mut self.dead, conn.as_handle().raw(), e.into());
        }
    }

    /// Take `width` by `height` pixels where the display sets modes, drawn at
    /// the scale it holds.
    fn set_mode(&mut self, width: u32, height: u32) -> Result<(), Refused> {
        if !self.display.sets_modes() {
            return Err(Refused::NoModes);
        }
        let mode = toyos_desktop::mode::follow(Mode::new(width, height)).map_err(|why| {
            println!("compositor: mode {width}x{height} is refused: {why}");
            Refused::OutOfBounds
        })?;
        let scale = scale_for(self.kept, self.image.unwrap_or(self.panel), mode);
        if self.change_mode(mode, scale) { Ok(()) } else { Err(Refused::Declined) }
    }

    /// Draw at `setting`, and keep it in this program's `HOME` for the next
    /// boot; a write that fails is said, and the scale is drawn at anyway.
    fn set_scale(&mut self, setting: Setting) -> Result<(), Refused> {
        let mode = self.display.mode();
        let kept = match setting {
            Setting::Auto => None,
            Setting::Kept(scale) => Some(Scale::asked(scale, (mode.width, mode.height)).map_err(|why| why.wire())?),
        };
        let path = kept_path();
        let written = match kept {
            Some(scale) => std::fs::write(&path, format!("{scale}\n")),
            None => std::fs::remove_file(&path).or_else(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(e),
            }),
        };
        if let Err(e) = written {
            eprintln!("compositor: {} is not written, so this scale is not kept: {e}", path.display());
        }
        self.kept = kept;
        let scale = scale_for(kept, self.image.unwrap_or(self.panel), mode);
        match kept {
            Some(k) => println!("compositor: scale {scale}, kept {k}"),
            None => println!("compositor: scale {scale}, auto"),
        }
        self.lay_out(mode, scale);
        Ok(())
    }

    /// Everything the desktop draws, laid out again for `mode` at `scale`:
    /// the back buffer, the desk and its furniture, the pointer's range, the
    /// window budget and the wallpaper; the font, the icons and the cursor at
    /// the scale; and every window placed by [`toyos_desktop::relayout`] and
    /// given a buffer of its new size — every one at a new scale, since its
    /// client draws at it. The whole screen is damaged.
    fn lay_out(&mut self, mode: Mode, scale: Scale) {
        let old = self.desk;
        let rescaled = scale != old.scale;
        if mode_rect(mode) != old.screen {
            self.back = Framebuffer::new(mode.width as usize, mode.height as usize, self.pixel_format());
            self.pointers.screen(mode.width, mode.height);
            self.wallpaper.rescale(mode, self.pixel_format());
        }
        self.desk = Desk::new(mode_rect(mode), scale, self.apps.len());
        if rescaled {
            self.font = self.shapes.font(&self.desk);
            self.icons = self.shapes.buttons(scale);
            self.cursors = self.shapes.cursors(scale);
            self.show_cursor();
        }
        // What a window costs moved, so what we can afford moved with it.
        // Windows already open are left alone if the new figure is below their
        // count — the cap gates creation, it does not evict.
        self.max_windows =
            toyos_desktop::max_windows(self.total_mem, self.desk.screen, MAX_WINDOW_SLOTS as usize);
        relayout(&old, &self.desk, &mut self.stack);
        let (pf, wire) = (self.pixel_format(), scale.wire());
        for win in self.stack.iter_mut() {
            let drawn = win.client.buffers.drawn();
            if rescaled || (drawn.width, drawn.height) != (win.content.w(), win.content.h()) {
                settle(win, pf, wire, &mut self.dead);
            }
        }
        self.cursor.x = self.cursor.x.min(self.desk.screen.x1 - 1);
        self.cursor.y = self.cursor.y.min(self.desk.screen.y1 - 1);
        self.damage.add(self.desk.screen);
    }

    /// The current cursor on the display's cursor plane where it carries it
    /// at this scale, and the plane emptied where the cursor is drawn in
    /// software.
    fn show_cursor(&mut self) {
        if !self.display.hw_cursor() {
            return;
        }
        let at = (self.cursor.x as u32, self.cursor.y as u32);
        if self.hw_cursor() {
            let (mask, color) = self.cursors.get(self.current_cursor);
            let hot = cursor::hot_spot(self.current_cursor, self.desk.scale);
            self.display.set_cursor(&render::cursor_image(mask, color), (hot.x as u32, hot.y as u32), at);
        } else {
            self.display.set_cursor(&[0; render::CURSOR_PIXELS], (0, 0), at);
        }
    }

    /// Whether the cursor is the display's: where it has a cursor plane and
    /// the sprite fits it at this scale.
    fn hw_cursor(&self) -> bool {
        self.display.hw_cursor() && cursor::in_hardware(self.desk.scale)
    }

    /// Take the mode the display's output now asks for, where it asks, at the
    /// scale its panel's size gives where nothing else is asked.
    fn follow_display(&mut self) {
        if let Some((mode, panel)) = self.display.followed() {
            let scale = scale_for(self.kept, self.image.unwrap_or(panel), mode);
            if self.change_mode(mode, scale) {
                self.panel = panel;
            }
        }
    }

    /// Flip to `mode` at `scale` (§5.7.6.3 on virtio-gpu): its scanout made,
    /// the whole desktop laid out and composed into it while the old mode is
    /// on screen, then shown. A mode the display declines leaves the old one,
    /// said; whether `mode` is on screen.
    fn change_mode(&mut self, mode: Mode, scale: Scale) -> bool {
        let (old, old_scale) = (self.display.mode(), self.desk.scale);
        if mode == old {
            if scale != old_scale {
                self.lay_out(mode, scale);
            }
            return true;
        }
        let pending = match self.display.prepare(mode) {
            Ok(pending) => pending,
            Err(why) => {
                println!("compositor: mode {mode} declined, {old} stays: {why}");
                return false;
            }
        };
        self.lay_out(mode, scale);
        let regions = self.compose();
        blit(&pending.screen, &self.back, &regions);
        match self.display.show(pending) {
            Ok(()) => {
                println!("compositor: mode {mode}");
                if self.hw_cursor() {
                    self.display.move_cursor((self.cursor.x as u32, self.cursor.y as u32));
                }
                self.frame_callbacks(&regions);
                true
            }
            Err(why) => {
                println!("compositor: mode {mode} declined, {old} stays: {why}");
                self.lay_out(old, old_scale);
                false
            }
        }
    }

    fn reap(&mut self) {
        announce(&self.dead);
        if self.dead.is_empty() {
            return;
        }
        let before = self.stack.len();
        // The rect a dropped window vacates is only knowable while it is still
        // in the list.
        let vacated: Vec<Rect> = self
            .stack
            .iter()
            .filter(|w| self.dead.iter().any(|(handle, _)| *handle == w.client.conn.as_handle().raw()))
            .map(|w| w.frame(&self.desk.chrome))
            .collect();
        let dead = std::mem::take(&mut self.dead);
        self.stack.retain(|w| !dead.iter().any(|(handle, _)| *handle == w.client.conn.as_handle().raw()));
        self.pending.retain(|p| !dead.iter().any(|(handle, _)| *handle == p.conn.as_handle().raw()));
        if self.watcher.as_ref().is_some_and(|w| dead.iter().any(|(handle, _)| *handle == w.conn.as_handle().raw())) {
            self.watcher = None;
            self.watch.leave();
        }
        self.dead = dead;
        if self.stack.len() != before {
            for rect in vacated {
                self.damage.add(rect);
            }
            self.damage_all();
        }
    }

    /// What the desktop session's owner said, and its control connection
    /// watched again, which a session opened again replaces.
    fn session_said(&mut self) {
        if self.desktop.said() {
            self.damage.add(sign_out_rect(&self.desk));
        }
        if let Some(control) = self.desktop.conn() {
            self.poller.watch(control, READABLE, control.as_handle().raw().0 as u64);
        }
    }

    /// Re-arm the one-shot poll registrations for every handle that fired.
    fn rearm(
        &mut self,
        inputs: bool,
        acceptor: bool,
        prompts: bool,
        clipboards: bool,
        settings: bool,
    ) {
        if let Some(handle) = self.display.handle().filter(|handle| self.is_ready(handle.raw())) {
            self.poller.watch(&handle, READABLE, handle.raw().0 as u64);
        }
        if let Some((conn, _)) = &self.volume {
            let handle = conn.as_handle().raw();
            if self.is_ready(handle) {
                self.poller.watch(conn, READABLE, handle.0 as u64);
            }
        }
        if inputs {
            self.poller.watch(&self.inputs, READABLE, self.inputs.as_handle().raw().0 as u64);
        }
        if prompts {
            self.poller.watch(&self.prompts, READABLE, self.prompts.as_handle().raw().0 as u64);
        }
        if clipboards {
            self.poller.watch(&self.clipboards, READABLE, self.clipboards.as_handle().raw().0 as u64);
        }
        if settings {
            self.poller.watch(&self.settings, READABLE, self.settings.as_handle().raw().0 as u64);
        }
        if let Some(w) = &self.watcher {
            let handle = w.conn.as_handle().raw();
            if self.is_ready(handle) {
                self.poller.watch(&w.conn, READABLE, handle.0 as u64);
            }
        }
        if acceptor {
            let h = self.acceptor.as_handle().raw();
            self.poller.watch(&self.acceptor, READABLE, h.0 as u64);
        }
        for win in self.stack.iter() {
            let handle = win.client.conn.as_handle().raw();
            if self.is_ready(handle) {
                self.poller.watch(&win.client.conn, READABLE, handle.0 as u64);
            }
        }
        for p in self.pending.iter() {
            let handle = p.conn.as_handle().raw();
            if self.is_ready(handle) {
                self.poller.watch(&p.conn, READABLE, handle.0 as u64);
            }
        }
    }

    fn tick_taskbar(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_taskbar_update) < Duration::from_secs(1) {
            return;
        }
        self.last_taskbar_update = now;

        let mut si = [0u8; 48];
        if system::sysinfo(&mut si) >= 48 {
            let total_mem = u64::from_le_bytes(si[0..8].try_into().unwrap());
            let used_mem = u64::from_le_bytes(si[8..16].try_into().unwrap());
            let busy = u64::from_le_bytes(si[32..40].try_into().unwrap());
            let total = u64::from_le_bytes(si[40..48].try_into().unwrap());
            let d_busy = busy.saturating_sub(self.prev_busy_ticks);
            let d_total = total.saturating_sub(self.prev_total_ticks);
            if d_total > 0 {
                self.cached_stats.cpu_pct = d_busy.saturating_mul(100) / d_total;
            }
            self.prev_busy_ticks = busy;
            self.prev_total_ticks = total;
            self.cached_stats.used_mb = used_mem / (1024 * 1024);
            self.cached_stats.total_mb = total_mem / (1024 * 1024);
        }

        // Only the readout, which is the only thing about the bar a second
        // changes. A whole-bar repaint here is what the owner saw as the
        // taskbar flickering once a second.
        self.damage.add(self.desk.taskbar(self.stack.len()).status());
    }

    fn present(&mut self) {
        // Two clock syscalls per composited frame — 120/s at the frame cap —
        // which is what any measure of a frame costs here.
        let started = Instant::now();
        let regions = self.compose();
        if regions.is_empty() {
            return;
        }
        let damage_px = blit(self.display.screen(), &self.back, &regions);
        self.totals.record(started.elapsed().as_nanos() as u64, regions.len(), damage_px);
        self.display.present(&regions);
        if let Some(Err(why)) = self.output.as_mut().map(|d| d.composed(&regions)) {
            self.drop_output(why);
        }
        if self.totals.frames() == 1 {
            println!("compositor: the first frame is on the panel");
        }
        self.frame_callbacks(&regions);
    }

    /// Compose every damaged region into the back buffer, and answer them.
    fn compose(&mut self) -> Vec<Rect> {
        if self.damage.is_empty() {
            return Vec::new();
        }
        let regions = self.damage.take(self.desk.screen);
        let assets = Assets {
            font: &self.font,
            icons: &self.icons,
            wallpaper: &self.wallpaper.scaled,
            apps: &self.apps,
        };
        for region in &regions {
            render::paint(
                &self.back,
                &self.desk,
                &self.stack,
                &assets,
                self.launcher_open,
                &self.cached_stats,
                *region,
            );
        }

        // Above every window and the taskbar, under the cursor.
        if let Some(shown) = self.overlay.visible(self.overlay_now()) {
            let rect = overlay_rect(&self.desk);
            if regions.iter().any(|r| r.overlaps(rect)) {
                render::draw_overlay(&self.back, &self.font, rect, &shown.text());
            }
        }
        if let Some(waiting) = &self.desktop.waiting {
            let rect = sign_out_rect(&self.desk);
            if regions.iter().any(|r| r.overlaps(rect)) {
                let text = sign_out_text(self.desktop.stopping, waiting.names.iter().map(String::as_str), waiting.more);
                render::draw_overlay(&self.back, &self.font, rect, &text);
            }
        }

        // Into the back buffer, so a region containing the cursor carries it
        // over with everything else rather than the panel being touched twice.
        if !self.hw_cursor() {
            let (mask, color) = self.cursors.get(self.current_cursor);
            let rect = cursor::drawn(self.current_cursor, self.cursor, self.desk.scale);
            if regions.iter().any(|r| r.overlaps(rect)) {
                render::draw_software_cursor(&self.back, mask, color, rect.origin());
            }
        }
        regions
    }

    /// Tell every window that presented and was composited that its frame is
    /// on the panel.
    fn frame_callbacks(&mut self, regions: &[Rect]) {
        let mut dead: Vec<Dead> = Vec::new();
        for i in 0..self.stack.len() {
            let rect = self.stack[i].frame(&self.desk.chrome);
            if self.stack[i].presented
                && !self.stack[i].minimized
                && regions.iter().any(|r| r.overlaps(rect))
            {
                deliver_signal(&mut dead, &self.stack[i], window::MSG_FRAME);
                self.stack[i].client.frames += 1;
                self.stack[i].presented = false;
                // The prompt's own first frame is on the panel: from here a
                // press can be an answer.
                if let Some((id, gate)) = &mut self.prompt {
                    if *id == self.stack[i].id && self.stack[i].client.frames == 1 {
                        gate.presented();
                        println!("compositor: the prompt is on the panel");
                    }
                }
            }
        }
        announce(&dead);
        if dead.is_empty() {
            return;
        }
        for win in
            self.stack.iter().filter(|w| dead.iter().any(|(h, _)| *h == w.client.conn.as_handle().raw()))
        {
            self.damage.add(win.frame(&self.desk.chrome));
        }
        self.stack.retain(|w| !dead.iter().any(|(handle, _)| *handle == w.client.conn.as_handle().raw()));
        self.damage_all();
    }

    /// Damage every window and the taskbar, for a change that reorders or
    /// re-focuses them.
    ///
    /// Bounded by what is on screen rather than by the screen: two small
    /// windows cost those two and the bar, where the full-screen repaint this
    /// replaces cost the wallpaper under everything as well. Minimized windows
    /// are damaged too — one of them may be the window that just stopped being
    /// minimized, and a caller that had to know which is a caller that will one
    /// day be wrong.
    fn damage_all(&mut self) {
        for i in 0..self.stack.len() {
            let rect = self.stack[i].frame(&self.desk.chrome);
            self.damage.add(rect);
        }
        self.damage.add(self.desk.taskbar(self.stack.len()).strip());
    }

    fn launcher_rect(&self) -> Rect {
        self.desk.taskbar(self.stack.len()).launcher()
    }

    /// Say so once the prompt is gone, however it went.
    fn prompt_gone(&mut self) {
        if let Some((id, gate)) = self.prompt {
            if self.stack.position(id).is_none() {
                self.prompt = None;
                self.owed = gate;
                println!("compositor: the prompt is down");
            }
        }
    }

    /// Start `program` through the launcher, off the loop.
    /// Start `program` through the desktop session's launcher, off the loop.
    fn launch(&mut self, program: &str) {
        self.desktop.launch(program);
    }

    fn retarget(&mut self, idx: usize, mode: WindowMode) {
        let pf = self.pixel_format();
        set_mode(&self.desk, &mut self.stack, idx, mode);
        settle(&mut self.stack[idx], pf, self.desk.scale.wire(), &mut self.dead);
    }

    /// GUI+V: hand the window at `idx` the clipboard.
    ///
    /// Past [`window::MAX_INLINE_PAYLOAD`] it goes through shared memory — a
    /// region made here and moved to the window with the message that says how
    /// much of it is text. The same number the window's receive buffer is.
    fn paste(&mut self, idx: usize) {
        if self.clipboard.is_empty() {
            return;
        }
        let win = &self.stack[idx];
        if self.clipboard.len() <= window::MAX_INLINE_PAYLOAD {
            if let Err(e) = win
                .client
                .conn
                .try_send_bytes(window::MSG_CLIPBOARD_PASTE, self.clipboard.as_bytes())
            {
                mark_dead(&mut self.dead, win.client.conn.as_handle().raw(), e.into());
            }
            return;
        }
        // Nothing is held here. The window's handle keeps the region alive once
        // the send has moved it, so the compositor's own mapping goes at the end
        // of this function — where the old code kept the last paste mapped until
        // the next one replaced it.
        let shm = match SharedMemory::create(self.clipboard.len()) {
            Ok(shm) => shm,
            Err(e) => {
                eprintln!(
                    "compositor: client {} gets no paste — no memory for {} bytes ({e:?})",
                    win.client.conn.as_handle().raw().0,
                    self.clipboard.len()
                );
                return;
            }
        };
        let Ok(handle) = shm.share_read_only() else {
            eprintln!(
                "compositor: client {} gets no paste — its region cannot be shared",
                win.client.conn.as_handle().raw().0
            );
            return;
        };
        shm.bytes().write(0, self.clipboard.as_bytes());
        deliver_with_handle(
            &mut self.dead,
            win,
            handle,
            window::MSG_CLIPBOARD_PASTE_SHM,
            &window::ClipboardShmMsg { len: self.clipboard.len() as u32 },
        );
    }
}

/// Bytes a client sent as clipboard text, validated only once no client can
/// change them.
fn text(from: RawHandle, bytes: Vec<u8>) -> Option<String> {
    String::from_utf8(bytes)
        .map_err(|e| {
            eprintln!(
                "compositor: refusing a clipboard from client {} — it is not UTF-8 ({})",
                from.0,
                e.utf8_error()
            )
        })
        .ok()
}

fn mode_rect(mode: Mode) -> Rect {
    Rect::new(0, 0, mode.width as i32, mode.height as i32)
}

/// Carry `regions` of the back buffer to `screen`, and answer the pixels.
fn blit(screen: &Screen, back: &Framebuffer, regions: &[Rect]) -> u64 {
    let mut pixels = 0;
    for region in regions {
        let (x, y) = (region.x0 as usize, region.y0 as usize);
        screen.blit(x, y, region.w() as usize, region.h() as usize, back, x, y);
        pixels += region.area();
    }
    pixels
}

/// The mode a window toggles into when it is maximized by button or chord.
fn toggled(mode: WindowMode) -> WindowMode {
    if mode == WindowMode::Normal {
        WindowMode::Maximized
    } else {
        WindowMode::Normal
    }
}

/// A cursor style off the wire.
///
/// A client can send any `u32`; one nobody implements is the default cursor,
/// not an index into anything.
fn cursor_from_wire(raw: u32) -> CursorStyle {
    match raw as u8 {
        window::CURSOR_CROSSHAIR => CursorStyle::Crosshair,
        window::CURSOR_RESIZE => CursorStyle::Resize,
        _ => CursorStyle::Default,
    }
}

fn mouse_event(
    win: &Win,
    at: Point,
    buttons: u8,
    event_type: u8,
    changed: u8,
    scroll: i8,
) -> window::MouseEvent {
    window::MouseEvent {
        x: (at.x - win.content.x0).max(0) as u16,
        y: (at.y - win.content.y0).max(0) as u16,
        buttons,
        event_type,
        changed,
        scroll,
    }
}

/// Give `win` a buffer the size of its content rect, to draw at `scale`, and
/// say whether it got one or is owed one. The buffer it was shown from stays
/// until its first present into the new one, and a window with a buffer
/// given and not yet presented is owed this one, given once it presents
/// ([`Buffers`]).
///
/// **Neither refusal is fatal.** The memory is the compositor's own rather than
/// the client's doing, so a refusal keeps the window at a size it can afford —
/// and a client that has exited is learned about by the send below refusing,
/// never by a grant naming a pid the process table no longer knows. That grant
/// is how the desktop used to die: `SharedMemory::grant` was infallible over an
/// `InvalidArgument` that a maximize could reach, and it took every other
/// window with it.
fn rebuffer(win: &mut Win, pixel_format: u32, scale: window::Scale, dead: &mut Vec<Dead>) -> bool {
    if win.client.buffers.owe() {
        return true;
    }
    let (w, h) = (win.content.w(), win.content.h());
    let (new_shm, client_shm) = match ReadOnly::create(w as usize * h as usize * 4) {
        Ok(made) => made,
        Err(e) => {
            eprintln!(
                "compositor: client {} keeps its {}x{} buffer — no region for {w}x{h} ({e:?})",
                win.client.conn.as_handle().raw().0,
                win.buf_w,
                win.buf_h
            );
            return false;
        }
    };
    let generation = win.client.buffers.give(Buffer { shm: new_shm, width: w, height: h, scale });
    // The one message a window cannot afford to miss — its next present is
    // read from the new buffer — so a client that will not take it is dropped
    // rather than left drawing into memory that is no longer shown.
    deliver_with_handle(
        dead,
        win,
        client_shm,
        window::MSG_WINDOW_RESIZED,
        &window::ResizeInfo {
            width: w as u32,
            height: h as u32,
            stride: w as u32,
            pixel_format,
            scale: scale.wire(),
            generation,
        },
    );
    true
}

/// Make the window and its buffer agree, whichever way round that has to go.
///
/// A window is allowed to run ahead of its memory only while a resize is being
/// dragged. Everywhere else the two must match, and if the machine will not
/// give the memory then it is the window that gives way.
fn settle(win: &mut Win, pixel_format: u32, scale: window::Scale, dead: &mut Vec<Dead>) {
    if !rebuffer(win, pixel_format, scale, dead) {
        win.content = win.backed();
    }
}
