//! The content-agnostic locker: acquires the `ext-session-lock`, creates one surface per output,
//! classifies each output's role, paints it via pluggable [`KioskContent`], routes keyboard input
//! to a password buffer, and unlocks the whole session on a correct PAM password. Fail-closed.

use crate::auth::{self, AuthOutcome};
use crate::content::{AuthState, AuthView, Frame, KioskContent, OutputRole};
use crate::lockscreen::ClockLockScreen;
use calloop::{
    channel::{channel, Event as ChanEvent},
    timer::{TimeoutAction, Timer},
    EventLoop,
};
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_registry, delegate_seat,
    delegate_session_lock, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        Capability, SeatHandler, SeatState,
    },
    session_lock::{
        SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface,
        SessionLockSurfaceConfigure,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard::WlKeyboard, wl_output, wl_seat::WlSeat, wl_shm, wl_surface},
    Connection, QueueHandle,
};
use zeroize::Zeroizing;

/// Per-host configuration. Values are supplied by the caller (from a config file in the default
/// binary, or in code by a library consumer) — the framework itself hardcodes no policy.
pub struct Config {
    /// Connector names (e.g. `"DP-3"`) that carry live kiosk content. Every other output is a
    /// `Session` lock screen — the safe default.
    pub kiosk_outputs: Vec<String>,
    /// PAM service name (`/etc/pam.d/<name>`). Fail-closed if missing.
    pub pam_service: String,
    /// Username to authenticate; `None` = the current `$USER`.
    pub username: Option<String>,
}

#[non_exhaustive]
#[derive(Debug)]
pub enum LockError {
    /// The compositor does not advertise `ext-session-lock-v1`; refuse loudly, never half-lock.
    NoSessionLockProtocol,
    Wayland(String),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LockError::NoSessionLockProtocol => write!(f, "compositor lacks ext-session-lock-v1"),
            LockError::Wayland(s) => write!(f, "wayland: {s}"),
        }
    }
}
impl std::error::Error for LockError {}

/// The entrypoint. Locks now, `ClockLockScreen` on `Session` outputs and `kiosk` on `Kiosk`
/// outputs, and blocks until the correct password unlocks the whole session.
pub fn run(config: Config, kiosk: impl KioskContent) -> Result<(), LockError> {
    run_inner(config, Box::new(kiosk), Box::new(ClockLockScreen::new()))
}

/// Full control — override the `Session` lock screen too.
pub fn builder(config: Config) -> Builder {
    Builder {
        config,
        kiosk: None,
        session: None,
    }
}

pub struct Builder {
    config: Config,
    kiosk: Option<Box<dyn KioskContent>>,
    session: Option<Box<dyn KioskContent>>,
}
impl Builder {
    pub fn kiosk(mut self, c: impl KioskContent) -> Self {
        self.kiosk = Some(Box::new(c));
        self
    }
    pub fn session(mut self, c: impl KioskContent) -> Self {
        self.session = Some(Box::new(c));
        self
    }
    pub fn run(self) -> Result<(), LockError> {
        let kiosk = self.kiosk.unwrap_or_else(|| Box::new(ClockLockScreen::new()));
        let session = self
            .session
            .unwrap_or_else(|| Box::new(ClockLockScreen::new()));
        run_inner(self.config, kiosk, session)
    }
}

fn run_inner(
    config: Config,
    kiosk_content: Box<dyn KioskContent>,
    session_content: Box<dyn KioskContent>,
) -> Result<(), LockError> {
    let username = config
        .username
        .clone()
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "root".to_string());
    eprintln!(
        "nixlock: kiosk_outputs={:?} pam={} user={username}",
        config.kiosk_outputs, config.pam_service
    );

    let conn = Connection::connect_to_env().map_err(|e| LockError::Wayland(e.to_string()))?;
    let (globals, mut event_queue) =
        registry_queue_init(&conn).map_err(|e| LockError::Wayland(e.to_string()))?;
    let qh: QueueHandle<Locker> = event_queue.handle();

    let compositor =
        CompositorState::bind(&globals, &qh).map_err(|e| LockError::Wayland(e.to_string()))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| LockError::Wayland(e.to_string()))?;
    let output_state = OutputState::new(&globals, &qh);
    let seat_state = SeatState::new(&globals, &qh);
    let session_lock_state = SessionLockState::new(&globals, &qh);

    let (attempt_tx, attempt_rx) = mpsc::channel::<Zeroizing<String>>();
    let (result_tx, result_ch) = channel::<AuthOutcome>();
    auth::spawn(config.pam_service.clone(), username, attempt_rx, result_tx);

    let mut locker = Locker {
        registry_state: RegistryState::new(&globals),
        output_state,
        seat_state,
        compositor,
        shm,
        session_lock_state,
        conn: conn.clone(),
        lock: None,
        pool: None,
        surfaces: Vec::new(),
        kiosk_outputs: config.kiosk_outputs,
        kiosk_content,
        session_content,
        keyboard: None,
        password: Zeroizing::new(String::new()),
        caps_lock: false,
        failed: false,
        verifying: false,
        attempt_tx,
        fail_count: 0,
        locked_until: None,
    };

    event_queue
        .roundtrip(&mut locker)
        .map_err(|e| LockError::Wayland(e.to_string()))?;
    event_queue
        .roundtrip(&mut locker)
        .map_err(|e| LockError::Wayland(e.to_string()))?;

    let lock = locker
        .session_lock_state
        .lock(&qh)
        .map_err(|_| LockError::NoSessionLockProtocol)?;
    locker.lock = Some(lock);

    let mut event_loop: EventLoop<Locker> =
        EventLoop::try_new().map_err(|e| LockError::Wayland(e.to_string()))?;
    let handle = event_loop.handle();
    WaylandSource::new(conn, event_queue)
        .insert(handle.clone())
        .map_err(|e| LockError::Wayland(e.to_string()))?;
    handle
        .insert_source(Timer::from_duration(Duration::from_secs(1)), |_, _, l| {
            // Sweep an expired wrong-password backoff here rather than a dedicated one-shot timer:
            // this 1s tick already runs unconditionally, so checking `Instant::now()` against
            // `locked_until` needs no extra `LoopHandle` stashed on `Locker`.
            if l.locked_until.is_some_and(|until| Instant::now() >= until) {
                l.locked_until = None;
            }
            l.redraw_all();
            TimeoutAction::ToDuration(Duration::from_secs(1))
        })
        .map_err(|e| LockError::Wayland(e.to_string()))?;
    handle
        .insert_source(result_ch, |ev, _, l| {
            if let ChanEvent::Msg(outcome) = ev {
                l.on_auth(outcome);
            }
        })
        .map_err(|e| LockError::Wayland(e.to_string()))?;

    event_loop
        .run(Duration::from_secs(1), &mut locker, |_| {})
        .map_err(|e| LockError::Wayland(e.to_string()))?;
    Ok(())
}

struct Entry {
    surface: SessionLockSurface,
    role: OutputRole,
    name: String,
    width: u32,
    height: u32,
}

struct Locker {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    compositor: CompositorState,
    shm: Shm,
    session_lock_state: SessionLockState,
    conn: Connection,
    lock: Option<SessionLock>,
    pool: Option<SlotPool>,
    surfaces: Vec<Entry>,
    kiosk_outputs: Vec<String>,
    kiosk_content: Box<dyn KioskContent>,
    session_content: Box<dyn KioskContent>,
    keyboard: Option<WlKeyboard>,
    password: Zeroizing<String>,
    caps_lock: bool,
    failed: bool,
    verifying: bool,
    attempt_tx: mpsc::Sender<Zeroizing<String>>,
    /// Consecutive `Wrong`/`MaxTries`/`Error` verdicts since the last (never-happened, since a
    /// success exits) reset. Drives the backoff delay below.
    fail_count: u32,
    /// Set on a failed verdict to `now + backoff`; while `Some` and still in the future, new
    /// attempts are refused (`submit`/`press_key`) so a wrong password cannot be retried faster
    /// than the delay. Cleared once expiry is observed (checked lazily, and swept by the existing
    /// 1s repaint timer) — never a lockout (`SESSION-1`: always retryable, just not instantly).
    locked_until: Option<Instant>,
}

impl Locker {
    fn auth_view(&self) -> AuthView {
        let state = if self.verifying {
            AuthState::Verifying
        } else if self.failed {
            AuthState::Failed
        } else {
            AuthState::Idle(self.password.chars().count())
        };
        AuthView {
            state,
            caps_lock: self.caps_lock,
        }
    }

    fn render_surface(&mut self, idx: usize) {
        let (wl, role, name, w, h) = {
            let e = &self.surfaces[idx];
            (
                e.surface.wl_surface().clone(),
                e.role,
                e.name.clone(),
                e.width,
                e.height,
            )
        };
        if w == 0 || h == 0 {
            return;
        }
        let av = self.auth_view();
        let expect = (w * h * 4) as usize;
        let rgba = {
            let frame = Frame {
                role,
                output_name: &name,
                width: w,
                height: h,
                auth: av,
            };
            match role {
                OutputRole::Kiosk => self.kiosk_content.paint(&frame),
                OutputRole::Session => self.session_content.paint(&frame),
            }
        };
        // KIOSK-2 / FRAME-1: a wrong-sized buffer is never blitted; fall back to the lock screen.
        let rgba = if rgba.len() == expect {
            rgba
        } else {
            eprintln!("nixlock: {name} content returned {} bytes (want {expect}); lock screen", rgba.len());
            let frame = Frame {
                role: OutputRole::Session,
                output_name: &name,
                width: w,
                height: h,
                auth: av,
            };
            self.session_content.paint(&frame)
        };
        if rgba.len() != expect {
            return; // even the fallback disagreed; skip rather than blit garbage
        }
        if self.pool.is_none() {
            self.pool = Some(SlotPool::new(expect, &self.shm).expect("pool"));
        }
        let pool = self.pool.as_mut().unwrap();
        let (buffer, canvas) = pool
            .create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888)
            .expect("buffer");
        for (dst, s) in canvas.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
            dst[0] = s[2];
            dst[1] = s[1];
            dst[2] = s[0];
            dst[3] = s[3];
        }
        buffer.attach_to(&wl).expect("attach");
        wl.damage_buffer(0, 0, w as i32, h as i32);
        wl.commit();
    }

    fn redraw_all(&mut self) {
        for idx in 0..self.surfaces.len() {
            self.render_surface(idx);
        }
    }

    fn redraw_locks(&mut self) {
        for idx in 0..self.surfaces.len() {
            if self.surfaces[idx].role == OutputRole::Session {
                self.render_surface(idx);
            }
        }
    }

    /// `true` while a wrong-password backoff delay is still running. Checked lazily against
    /// `Instant::now()` — no dedicated expiry callback needed, since a `false` result here is
    /// exactly as good as an eagerly-cleared `None` (see the repaint timer for the sweep that keeps
    /// `locked_until` from growing stale forever).
    fn backoff_active(&self) -> bool {
        self.locked_until.is_some_and(|until| Instant::now() < until)
    }

    fn submit(&mut self) {
        if self.verifying || self.backoff_active() || self.password.is_empty() {
            return;
        }
        let pw = std::mem::take(&mut self.password);
        self.verifying = true;
        self.failed = false;
        self.redraw_locks();
        let _ = self.attempt_tx.send(pw);
    }

    fn on_auth(&mut self, outcome: AuthOutcome) {
        self.verifying = false;
        match outcome {
            AuthOutcome::Unlocked => {
                if let Some(lock) = self.lock.take() {
                    lock.unlock();
                }
                let _ = self.conn.flush(); // push unlock_and_destroy before exit
                std::process::exit(0);
            }
            AuthOutcome::Wrong | AuthOutcome::MaxTries | AuthOutcome::Error => {
                self.failed = true;
                self.password.clear();
                // Growing backoff before the NEXT attempt is accepted (SESSION-1: always
                // retryable, just not instantly) -- 1s,2s,3s,4s,5s, capped at 5s.
                self.fail_count = self.fail_count.saturating_add(1);
                let delay = Duration::from_secs(self.fail_count.min(5) as u64);
                self.locked_until = Some(Instant::now() + delay);
                self.redraw_locks();
            }
        }
    }
}

impl SessionLockHandler for Locker {
    fn locked(&mut self, _conn: &Connection, qh: &QueueHandle<Self>, session_lock: SessionLock) {
        // Create Session (lock-screen) outputs FIRST so keyboard focus lands on a lock screen,
        // never on a kiosk output (KIOSK-1: kiosk surfaces are input-inert).
        let mut ordered: Vec<(wl_output::WlOutput, String, OutputRole)> = self
            .output_state
            .outputs()
            .map(|o| {
                let name = self
                    .output_state
                    .info(&o)
                    .and_then(|i| i.name)
                    .unwrap_or_default();
                let role = if self.kiosk_outputs.iter().any(|k| k == &name) {
                    OutputRole::Kiosk
                } else {
                    OutputRole::Session
                };
                (o, name, role)
            })
            .collect();
        ordered.sort_by_key(|(_, _, role)| *role == OutputRole::Kiosk); // Session (false) first
        eprintln!("nixlock: locked; {} output(s)", ordered.len());
        for (output, name, role) in ordered {
            let surface = self.compositor.create_surface(qh);
            let lock_surface = session_lock.create_lock_surface(surface, &output, qh);
            eprintln!("nixlock:   '{name}' -> {role:?}");
            self.surfaces.push(Entry {
                surface: lock_surface,
                role,
                name,
                width: 0,
                height: 0,
            });
        }
    }

    fn finished(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _lock: SessionLock) {
        eprintln!("nixlock: lock finished/rejected");
        std::process::exit(1);
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: SessionLockSurface,
        configure: SessionLockSurfaceConfigure,
        _serial: u32,
    ) {
        let (w, h) = configure.new_size;
        if let Some(idx) = self
            .surfaces
            .iter()
            .position(|e| e.surface.wl_surface() == surface.wl_surface())
        {
            self.surfaces[idx].width = w;
            self.surfaces[idx].height = h;
            self.render_surface(idx);
        }
    }
}

impl SeatHandler for Locker {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: WlSeat, cap: Capability) {
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            match self.seat_state.get_keyboard(qh, &seat, None) {
                Ok(kb) => self.keyboard = Some(kb),
                Err(e) => eprintln!("nixlock: get_keyboard failed: {e}"),
            }
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, cap: Capability) {
        if cap == Capability::Keyboard {
            if let Some(kb) = self.keyboard.take() {
                kb.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl KeyboardHandler for Locker {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlKeyboard, _: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlKeyboard, _: &wl_surface::WlSurface, _: u32) {}
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlKeyboard, _: u32, event: KeyEvent) {
        if self.verifying || self.backoff_active() {
            return; // one attempt in flight (AUTH-2), or cooling down after a wrong one
        }
        match event.keysym {
            Keysym::Return | Keysym::KP_Enter => self.submit(),
            Keysym::BackSpace => {
                self.password.pop();
                self.failed = false;
                self.redraw_locks();
            }
            Keysym::Escape => {
                self.password.clear();
                self.failed = false;
                self.redraw_locks();
            }
            _ => {
                if let Some(txt) = event.utf8 {
                    if !txt.is_empty() && !txt.chars().any(|c| c.is_control()) {
                        self.password.push_str(&txt);
                        self.failed = false;
                        self.redraw_locks();
                    }
                }
            }
        }
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlKeyboard, _: u32, _: KeyEvent) {}
    fn repeat_key(&mut self, c: &Connection, q: &QueueHandle<Self>, k: &WlKeyboard, s: u32, e: KeyEvent) {
        self.press_key(c, q, k, s, e);
    }
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlKeyboard, _: u32, mods: Modifiers, _: RawModifiers, _: u32) {
        if mods.caps_lock != self.caps_lock {
            self.caps_lock = mods.caps_lock;
            self.redraw_locks();
        }
    }
}

impl CompositorHandler for Locker {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}
impl OutputHandler for Locker {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}
impl ShmHandler for Locker {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}
impl ProvidesRegistryState for Locker {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}
delegate_compositor!(Locker);
delegate_output!(Locker);
delegate_shm!(Locker);
delegate_seat!(Locker);
delegate_keyboard!(Locker);
delegate_session_lock!(Locker);
delegate_registry!(Locker);
