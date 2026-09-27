//! An animation through winit's own API: every `RedrawRequested` draws a frame,
//! calls `pre_present_notify`, presents, and asks for the next redraw at once,
//! as iced's `RedrawRequest::NextFrame` and every animated toolkit do.
//!
//! What it measures is not here: the compositor counts the window's presents
//! and the frame events it sent back, and says both when the window closes,
//! which the harness does with GUI+Q once the animation says it is done.
//! Paced, a redraw waits for the frame event of the present before it, so the
//! presents can outrun the frames by the one still on its way; unpaced, the
//! application draws as fast as the CPU goes and the compositor answers only
//! as often as it composes.

use std::num::NonZeroU32;
use std::sync::Arc;

use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop, OwnedDisplayHandle};
use winit::window::{Window, WindowAttributes, WindowId};

/// Frames drawn before the window closes itself.
const FRAMES: u32 = 60;

struct Animation {
    window: Arc<Window>,
    surface: Surface<OwnedDisplayHandle, Arc<Window>>,
    drawn: u32,
}

struct App {
    context: Context<OwnedDisplayHandle>,
    animation: Option<Animation>,
}

fn fail(what: &str) -> ! {
    println!("WINIT-PACE-FAIL {what}");
    std::process::exit(1);
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = WindowAttributes::default()
            .with_title("animation")
            .with_inner_size(winit::dpi::PhysicalSize::new(240u32, 160u32));
        let window =
            Arc::new(event_loop.create_window(attrs).unwrap_or_else(|e| fail(&format!("create: {e}"))));
        let surface = Surface::new(&self.context, window.clone())
            .unwrap_or_else(|e| fail(&format!("surface: {e}")));
        self.animation = Some(Animation { window, surface, drawn: 0 });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(animation) = self.animation.as_mut() else { return };
        if event == WindowEvent::CloseRequested {
            event_loop.exit();
            return;
        }
        if event != WindowEvent::RedrawRequested || animation.drawn == FRAMES {
            return;
        }
        let size = animation.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            fail("a window with no size")
        };
        animation.surface.resize(w, h).unwrap_or_else(|e| fail(&format!("resize: {e}")));
        let mut buffer =
            animation.surface.buffer_mut().unwrap_or_else(|e| fail(&format!("buffer: {e}")));
        let shade = (animation.drawn * 255 / FRAMES) & 0xff;
        buffer.fill(shade << 16 | 0x40 << 8 | (255 - shade));
        animation.window.pre_present_notify();
        buffer.present().unwrap_or_else(|e| fail(&format!("present: {e}")));
        animation.drawn += 1;
        if animation.drawn < FRAMES {
            animation.window.request_redraw();
        } else {
            println!("WINIT-PACE drew {FRAMES} frames");
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().unwrap_or_else(|e| fail(&format!("event loop: {e}")));
    let context = Context::new(event_loop.owned_display_handle())
        .unwrap_or_else(|e| fail(&format!("softbuffer context: {e}")));
    let mut app = App { context, animation: None };
    event_loop.run_app(&mut app).unwrap_or_else(|e| fail(&format!("run: {e}")));
}
