//! The public content interface. nixlock is content-agnostic: it owns the lock, the per-output
//! surfaces, keyboard input and PAM auth, and asks a [`KioskContent`] to paint each output. Both
//! roles are content — the shipped [`crate::ClockLockScreen`] is just the default `Session` content.

use std::time::Duration;

/// Which lock surface an output carries. `Kiosk` = live pluggable content (a dashboard) with no
/// password affordance; `Session` = the PAM lock screen. Anything not named a kiosk output is
/// `Session` — the SAFE default, so a newly-plugged or unknown monitor shows the lock screen,
/// never the dashboard.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputRole {
    Kiosk,
    Session,
}

/// What the lock screen may render about the current auth attempt. The password itself is NEVER
/// here — only its shape. `Kiosk` content ignores this.
#[derive(Clone, Copy, Debug)]
pub enum AuthState {
    /// No attempt in progress; `usize` is how many characters are currently typed.
    Idle(usize),
    /// A PAM attempt is running off-thread; input is inert until it resolves.
    Verifying,
    /// The last attempt was rejected.
    Failed,
}

/// The auth view handed to content each frame (meaningful only on `Session` outputs).
#[derive(Clone, Copy, Debug)]
pub struct AuthView {
    pub state: AuthState,
    pub caps_lock: bool,
}

/// Everything content needs to paint one output for one tick. No wayland, no shm, no auth
/// internals leak through — content stays a pure `(state, size) -> pixels` function.
pub struct Frame<'a> {
    pub role: OutputRole,
    pub output_name: &'a str,
    pub width: u32,
    pub height: u32,
    pub auth: AuthView,
}

/// Pluggable per-output content. It runs inside the locker (`Send + 'static`).
pub trait KioskContent: Send + 'static {
    /// Paint one output. Return PREMULTIPLIED RGBA8, exactly `width * height * 4` bytes. The
    /// framework owns the BGRA swizzle and the `wl_shm` commit; a wrong-sized buffer is rejected
    /// (never blitted), and the output falls back to the lock screen.
    fn paint(&mut self, frame: &Frame) -> Vec<u8>;

    /// How often to repaint absent input (clock tick / poll cadence).
    fn tick_interval(&self) -> Duration {
        Duration::from_secs(1)
    }
}
