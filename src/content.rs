//! The content interface nixlock paints INTERNALLY: the `Session` (password) lock screen, always,
//! and the `Kiosk` output's fallback clock whenever the socket has no live frame for it (see
//! `crate::socket`). It is no longer a per-output plug-in API for kiosk content — that content now
//! arrives over the Unix socket as raw premultiplied RGBA, content-blind to nixlock. A library
//! consumer's only remaining lever here is overriding the `Session` look via [`crate::builder`];
//! the shipped [`crate::ClockLockScreen`] is the default for both `Session` and the `Kiosk`
//! fallback.

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
