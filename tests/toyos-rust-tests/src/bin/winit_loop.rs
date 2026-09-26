//! The ToyOS winit backend's event loop, through winit's own API: every source
//! of work that does not arrive on a window's connection reaches a loop that
//! is waiting, and nothing is delivered for a window once it is gone.
//!
//! Five stages, one after another, each ended by what it waits for or failed
//! by [`CEILING`], a liveness bound only a lost wake reaches:
//!
//! 1. A user event sent from `AboutToWait`, with no window open: the wake it
//!    raises is taken by the next iteration, never by the one that sent it.
//! 2. [`HELPER_EVENTS`] user events from another thread, each sent only once
//!    the one before it was delivered, so each needs a wake of its own.
//! 3. [`HELPER_ROUNDS`] windows handed to another thread, which asks each for a
//!    redraw and then drops it, each just as the loop goes to wait: the redraw
//!    and the `Destroyed` both have to reach it.
//! 4. A window created, asked for a redraw and dropped in one handler: its
//!    `Destroyed` arrives and no `RedrawRequested` follows it.
//! 5. A window whose close the application ignores: once the compositor has
//!    closed it, the loop does not wake for it while it waits out
//!    [`IDLE_WINDOW`]. The harness closes it with GUI+Q when told
//!    `WINIT-LOOP CLOSE-ME`.

use std::num::NonZeroU32;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy, OwnedDisplayHandle};
use winit::window::{Window, WindowAttributes, WindowId};

/// A liveness ceiling, not a duration: a delivered event ends each wait at
/// once, and only a lost wake reaches it.
const CEILING: Duration = Duration::from_secs(20);

const HELPER_EVENTS: u32 = 100;

/// Rounds, because a push that lands before the loop has looked at its queues
/// needs no wake: over this many, one that lands after is certain.
const HELPER_ROUNDS: u32 = 20;

/// How long stage 5 watches a loop with nothing to deliver. A measurement
/// window rather than a wait for an event: what is counted is what happens
/// when nothing does.
const IDLE_WINDOW: Duration = Duration::from_secs(1);

/// The iterations an idle loop may start in [`IDLE_WINDOW`]: the one that
/// ends it at its deadline, and one `WaitCancelled` straight after the close,
/// which winit allows to be spurious. A loop that waits on a closed
/// connection wakes for as long as the window lasts.
const IDLE_WAKES: usize = 2;

enum Ev {
    Ping,
    Seq(u32),
}

type Job = Box<dyn FnOnce() + Send>;

enum Stage {
    Ping { sent: bool },
    Helper { next: u32, ack: mpsc::Sender<()> },
    Handed { round: u32, step: Handed },
    Dropped { id: WindowId, destroyed: bool },
    Close { window: Option<Arc<Window>>, surface: Option<Surface<OwnedDisplayHandle, Arc<Window>>>, idle: Option<(Instant, Vec<String>)> },
}

enum Handed {
    /// The window is new; its first redraw is the one its creation queued.
    Created(Arc<Window>),
    /// The helper has been told to ask this window for a redraw.
    RedrawAsked(Arc<Window>),
    /// The helper has been told to drop it.
    DropAsked(WindowId),
}

struct App {
    proxy: EventLoopProxy<Ev>,
    jobs: mpsc::Sender<Job>,
    context: Context<OwnedDisplayHandle>,
    stage: Stage,
    /// A job for the helper, sent from `AboutToWait` so it lands as the loop
    /// goes to wait.
    queued: Option<Job>,
}

fn fail(what: &str) -> ! {
    println!("WINIT-LOOP-FAIL {what}");
    std::process::exit(1);
}

impl App {
    fn create(&self, event_loop: &ActiveEventLoop, title: &str) -> Arc<Window> {
        let attrs = WindowAttributes::default()
            .with_title(title)
            .with_inner_size(winit::dpi::PhysicalSize::new(200u32, 120u32));
        Arc::new(event_loop.create_window(attrs).unwrap_or_else(|e| fail(&format!("create: {e}"))))
    }

    fn hand_round(&mut self, event_loop: &ActiveEventLoop, round: u32) {
        let window = self.create(event_loop, "handed");
        self.stage = Stage::Handed { round, step: Handed::Created(window) };
    }

    fn stage_4(&mut self, event_loop: &ActiveEventLoop) {
        let window = self.create(event_loop, "dropped");
        let id = window.id();
        window.request_redraw();
        drop(window);
        self.stage = Stage::Dropped { id, destroyed: false };
    }

    fn stage_5(&mut self, event_loop: &ActiveEventLoop) {
        let window = self.create(event_loop, "ignores its close");
        let surface = Surface::new(&self.context, window.clone())
            .unwrap_or_else(|e| fail(&format!("surface: {e}")));
        self.stage = Stage::Close { window: Some(window), surface: Some(surface), idle: None };
    }
}

impl ApplicationHandler<Ev> for App {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if let Stage::Close { idle: Some((deadline, wakes)), .. } = &mut self.stage {
            let left = deadline.saturating_duration_since(Instant::now());
            wakes.push(format!("{cause:?} with {left:?} of the window left"));
            return;
        }
        if let StartCause::ResumeTimeReached { .. } = cause {
            let at = match &self.stage {
                Stage::Ping { .. } => "the user event sent from AboutToWait".to_string(),
                Stage::Helper { next, .. } => format!("helper event {next}"),
                Stage::Handed { round, step: Handed::Created(_) } => format!("round {round}'s first redraw"),
                Stage::Handed { round, step: Handed::RedrawAsked(_) } => {
                    format!("round {round}'s redraw asked from the helper")
                }
                Stage::Handed { round, step: Handed::DropAsked(_) } => {
                    format!("round {round}'s window dropped on the helper")
                }
                Stage::Dropped { .. } => "the Destroyed of a window dropped in its handler".to_string(),
                Stage::Close { .. } => "the compositor's close".to_string(),
            };
            fail(&format!("LOST: the loop waited out its ceiling for {at}"));
        }
    }

    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Ev) {
        match (&mut self.stage, event) {
            (Stage::Ping { sent: true }, Ev::Ping) => {
                println!("WINIT-LOOP stage 1: a user event sent from AboutToWait was delivered");
                let (ack, acked) = mpsc::channel::<()>();
                let proxy = self.proxy.clone();
                self.jobs
                    .send(Box::new(move || {
                        for i in 0..HELPER_EVENTS {
                            if proxy.send_event(Ev::Seq(i)).is_err() {
                                fail("the loop closed under the helper");
                            }
                            if acked.recv_timeout(CEILING).is_err() {
                                fail(&format!("LOST: helper event {i} was never delivered"));
                            }
                        }
                    }))
                    .expect("the helper outlives the loop");
                self.stage = Stage::Helper { next: 0, ack };
            }
            (Stage::Helper { next, ack }, Ev::Seq(i)) => {
                if i != *next {
                    fail(&format!("helper event {i} arrived when {next} was due"));
                }
                *next += 1;
                ack.send(()).expect("the helper waits for every ack");
                if *next == HELPER_EVENTS {
                    println!("WINIT-LOOP stage 2: {HELPER_EVENTS} events from another thread each woke the loop");
                    self.hand_round(event_loop, 0);
                }
            }
            (_, Ev::Ping) => fail("a ping out of turn"),
            (_, Ev::Seq(i)) => fail(&format!("helper event {i} out of turn")),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        match &mut self.stage {
            Stage::Handed { round, step } => {
                let round = *round;
                match (std::mem::replace(step, Handed::DropAsked(id)), event) {
                    (Handed::Created(w), WindowEvent::RedrawRequested) if w.id() == id => {
                        let asked = w.clone();
                        self.queued = Some(Box::new(move || asked.request_redraw()));
                        *step = Handed::RedrawAsked(w);
                    }
                    (Handed::RedrawAsked(w), WindowEvent::RedrawRequested) if w.id() == id => {
                        self.queued = Some(Box::new(move || drop(w)));
                        *step = Handed::DropAsked(id);
                    }
                    (Handed::DropAsked(gone), WindowEvent::Destroyed) if gone == id => {
                        if round + 1 < HELPER_ROUNDS {
                            self.hand_round(event_loop, round + 1);
                        } else {
                            println!(
                                "WINIT-LOOP stage 3: {HELPER_ROUNDS} redraws asked and windows dropped \
                                 on another thread each reached the loop"
                            );
                            self.stage_4(event_loop);
                        }
                    }
                    (Handed::DropAsked(gone), WindowEvent::RedrawRequested) if gone == id => {
                        fail(&format!("round {round}: RedrawRequested for a window already dropped"));
                    }
                    (prior, _) => *step = prior,
                }
            }
            Stage::Dropped { id: gone, destroyed } if *gone == id => match event {
                WindowEvent::Destroyed => *destroyed = true,
                WindowEvent::RedrawRequested => fail(if *destroyed {
                    "RedrawRequested after Destroyed"
                } else {
                    "RedrawRequested for a window its application already dropped"
                }),
                _ => {}
            },
            Stage::Close { window: Some(window), surface: Some(surface), idle } if window.id() == id => {
                match event {
                    WindowEvent::RedrawRequested => {
                        let size = window.inner_size();
                        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                        else {
                            fail("a window with no size")
                        };
                        surface.resize(w, h).unwrap_or_else(|e| fail(&format!("resize: {e}")));
                        let mut buffer = surface.buffer_mut().unwrap_or_else(|e| fail(&format!("buffer: {e}")));
                        buffer.fill(0x0030_6090);
                        buffer.present().unwrap_or_else(|e| fail(&format!("present: {e}")));
                        println!("WINIT-LOOP CLOSE-ME");
                    }
                    WindowEvent::CloseRequested if idle.is_none() => {
                        // Ignored, as an application that asks "save first?" does.
                        *idle = Some((Instant::now() + IDLE_WINDOW, Vec::new()));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Stage::Ping { sent } = &mut self.stage {
            if !*sent {
                *sent = true;
                if self.proxy.send_event(Ev::Ping).is_err() {
                    fail("the loop closed under its own proxy");
                }
            }
        }
        if let Stage::Dropped { destroyed: true, .. } = self.stage {
            println!("WINIT-LOOP stage 4: a window dropped in its handler got Destroyed and no redraw after it");
            self.stage_5(event_loop);
        }
        if let Some(job) = self.queued.take() {
            self.jobs.send(job).expect("the helper outlives the loop");
        }
        match &mut self.stage {
            Stage::Close { idle: Some((deadline, wakes)), window, surface } => {
                if Instant::now() < *deadline {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(*deadline));
                    return;
                }
                let count = wakes.len();
                if count > IDLE_WAKES {
                    let first: Vec<&String> = wakes.iter().take(8).collect();
                    fail(&format!(
                        "a closed window the application kept woke the loop {count} times in \
                         {IDLE_WINDOW:?}, first {first:?}"
                    ));
                }
                println!(
                    "WINIT-LOOP stage 5: a closed window the application kept woke the loop {count} \
                     time(s) in {IDLE_WINDOW:?}: {wakes:?}"
                );
                surface.take();
                window.take();
                println!("WINIT-LOOP-OK");
                event_loop.exit();
            }
            _ => event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + CEILING)),
        }
    }
}

fn main() {
    let event_loop = EventLoop::<Ev>::with_user_event()
        .build()
        .unwrap_or_else(|e| fail(&format!("event loop: {e}")));
    let context = Context::new(event_loop.owned_display_handle())
        .unwrap_or_else(|e| fail(&format!("softbuffer context: {e}")));
    let (jobs, work) = mpsc::channel::<Job>();
    thread::spawn(move || {
        for job in work {
            job();
        }
    });
    let mut app = App {
        proxy: event_loop.create_proxy(),
        jobs,
        context,
        stage: Stage::Ping { sent: false },
        queued: None,
    };
    event_loop.run_app(&mut app).unwrap_or_else(|e| fail(&format!("run: {e}")));
}
