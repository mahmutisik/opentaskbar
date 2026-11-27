//! Display-paced frames for CPU-driven animations (the magnification springs). A small thread waits for each DWM
//! composition pass and posts one message to the window; it sleeps on a condition variable while no animation runs,
//! so an idle dock uses no CPU. At most one frame message is queued at a time.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use windows_sys::Win32::Graphics::Dwm::DwmFlush;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

#[derive(Default)]
struct Shared {
    /// (running, shutting down)
    state: Mutex<(bool, bool)>,
    wake: Condvar,
    /// A frame message is in the queue and not handled yet.
    queued: AtomicBool,
}

pub struct FrameClock {
    shared: Arc<Shared>,
}

impl FrameClock {
    pub fn new(hwnd: isize, message: u32) -> Self {
        let shared = Arc::new(Shared::default());
        let worker = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("ot-frames".into())
            .spawn(move || {
                loop {
                    {
                        let mut state = worker.state.lock().unwrap_or_else(|e| e.into_inner());
                        while !state.0 && !state.1 {
                            state = worker.wake.wait(state).unwrap_or_else(|e| e.into_inner());
                        }
                        if state.1 {
                            return;
                        }
                    }
                    // SAFETY: blocks until the next composition pass; fails only without DWM.
                    if unsafe { DwmFlush() } < 0 {
                        std::thread::sleep(std::time::Duration::from_millis(16));
                    }
                    if !worker.queued.swap(true, Ordering::AcqRel) {
                        // SAFETY: posting to the dock window; a destroyed window just drops the message.
                        unsafe { PostMessageW(hwnd as _, message, 0, 0) };
                    }
                }
            });
        if let Err(error) = spawned {
            ot_core::log::warn(format!("Frame clock thread failed: {error}"));
        }
        Self { shared }
    }

    pub fn set_running(&self, running: bool) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.0 != running {
            state.0 = running;
            self.shared.wake.notify_one();
        }
    }

    /// Call when the frame message is handled, so the next one can be posted.
    pub fn frame_handled(&self) {
        self.shared.queued.store(false, Ordering::Release);
    }
}

impl Drop for FrameClock {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.1 = true;
        self.shared.wake.notify_one();
    }
}
