//! The kiosk display SOCKET SERVER: a content-BLIND Unix-domain frame receiver. Exactly one
//! external client at a time (`nixwatch-frames`, or anything speaking the same wire protocol)
//! streams premultiplied RGBA frames in; the render path (`locker.rs`) blits the latest one onto
//! the KIOSK output and falls back to nixlock's own built-in clock whenever no frame has ever
//! arrived, the frame's geometry does not match the kiosk output, or the client has disconnected.
//!
//! WIRE PROTOCOL v1 (all integers little-endian):
//!   - on connect, this server immediately writes a HELLO: 8-byte magic `b"NIXLOCK1"`, then
//!     `width: u32`, `height: u32`, `scale: u32` -- the kiosk output's current geometry. `0,0,0`
//!     means "no kiosk output" (either none is configured on this host, or the compositor has not
//!     sent its first surface `configure` yet -- see `SocketState::geometry`'s own doc).
//!   - the client then sends repeated FRAMES: `width: u32`, `height: u32`, then exactly
//!     `width*height*4` bytes of premultiplied RGBA (row-major, top-down, stride = width*4).
//!   - a frame whose geometry does not match the kiosk output is dropped (never blitted) and the
//!     connection is kept; a genuinely truncated read (the stream itself ended mid-frame) is
//!     treated the same as a clean disconnect, since there is no way to resynchronize a byte
//!     stream without knowing how much of it to discard.
//!
//! DISPLAY-ONLY, PERMANENTLY (BEHAVIORS.md DISPLAY-2): this module only ever turns received bytes
//! into pixels for one output. It has no path to `Locker`'s password buffer, auth state, or the
//! session-lock unlock call -- SESSION-1 stays the only gate, entirely untouched by anything here.
//! A reverse channel (input events client-ward, so a streamed dashboard could itself be
//! interactive) is a plausible future addition -- deliberately NOT built here; it would need its
//! own careful accounting against KIOSK-1 (kiosk surfaces are input-inert) before it could exist
//! at all.
use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub const MAGIC: &[u8; 8] = b"NIXLOCK1";

/// A frame this large is never legitimate for a real display, at any DPI this framework will ever
/// run on -- guards against a garbled or hostile length prefix turning into a multi-gigabyte
/// allocation. Purely a defensive backstop; the normal rejection path is the geometry check below.
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// The most recently fully-received, geometry-valid frame -- or `None` while no client has ever
/// sent one, or after the streaming client disconnects.
pub struct LatestFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Shared state the render path (`locker.rs`) reads and this module's accept/read threads write.
pub struct SocketState {
    pub latest: Mutex<Option<LatestFrame>>,
    /// Current kiosk output geometry (width, height, scale) -- `(0, 0, 0)` means "no kiosk
    /// output": either `kiosk_outputs` is empty (permanent, by construction) or the compositor
    /// has not yet sent the kiosk surface's first `configure` (a narrow startup race -- a client
    /// connecting in that window sees `0,0,0` too and should idle/retry exactly as it would for
    /// the permanent case; see the wire protocol doc above).
    geometry: Mutex<(u32, u32, u32)>,
    /// Set once the caller's calloop event loop exists, so a newly-accepted frame can wake a
    /// repaint immediately instead of waiting for the 1s tick. `None` early at startup is fine --
    /// the periodic tick still repaints eventually (see `locker.rs`'s own timer).
    ping: Mutex<Option<calloop::ping::Ping>>,
    /// Monotonic id of the current connection; a superseded (older) connection's handler thread
    /// checks this and exits rather than going on writing frames nobody will read.
    active_gen: AtomicU64,
    /// A clone of the currently-active stream, kept ONLY so a new connection can `shutdown()` the
    /// previous one -- unblocking its handler thread out of a pending `read` (one client at a
    /// time: a new connection replaces the old, per the protocol doc in `locker.rs`/README).
    active_stream: Mutex<Option<UnixStream>>,
}

impl SocketState {
    pub fn new() -> Arc<Self> {
        Arc::new(SocketState {
            latest: Mutex::new(None),
            geometry: Mutex::new((0, 0, 0)),
            ping: Mutex::new(None),
            active_gen: AtomicU64::new(0),
            active_stream: Mutex::new(None),
        })
    }

    /// Called by `locker.rs` whenever the kiosk output's surface configures (or reconfigures).
    pub fn set_geometry(&self, width: u32, height: u32, scale: u32) {
        *self.geometry.lock().unwrap() = (width, height, scale);
    }

    /// Called once the caller's `EventLoop` and its ping source exist.
    pub fn set_ping(&self, ping: calloop::ping::Ping) {
        *self.ping.lock().unwrap() = Some(ping);
    }
}

/// Resolve the socket path: the configured value, else `$XDG_RUNTIME_DIR/nixlock.sock`. `None`
/// when neither is available -- the caller then skips the socket server entirely. A display
/// feature must never block or weaken the lock itself (DISPLAY-2), so a missing/misconfigured
/// socket is a silent-degrade-to-clock, never a startup failure.
pub fn resolve_path(configured: Option<&Path>) -> Option<PathBuf> {
    configured.map(Path::to_path_buf).or_else(|| {
        std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("nixlock.sock"))
    })
}

/// Starts the listener thread. Best-effort: a bind failure is logged and the kiosk stays on the
/// default clock forever (never a reason to fail the lock itself -- see `resolve_path` above).
pub fn spawn(path: PathBuf, state: Arc<SocketState>) {
    std::thread::spawn(move || {
        // Unlink a stale socket from a previous, uncleanly-exited run before binding -- otherwise
        // `bind` fails with AddrInUse against a path nothing is actually listening on any more.
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                eprintln!(
                    "nixlock: socket: bind {} failed: {e}; kiosk socket disabled, default clock only",
                    path.display()
                );
                return;
            }
        };
        eprintln!("nixlock: socket: listening on {}", path.display());
        for stream in listener.incoming() {
            match stream {
                Ok(s) => accept_one(s, &state),
                Err(e) => eprintln!("nixlock: socket: accept failed: {e}"),
            }
        }
    });
}

fn accept_one(mut stream: UnixStream, state: &Arc<SocketState>) {
    // One client at a time: this connection becomes the new active one, and whatever was active
    // before it gets shut down so its handler thread unblocks out of its read and exits cleanly.
    let my_gen = state.active_gen.fetch_add(1, Ordering::SeqCst) + 1;
    if let Some(old) = state.active_stream.lock().unwrap().take() {
        let _ = old.shutdown(Shutdown::Both);
    }

    let (w, h, scale) = *state.geometry.lock().unwrap();
    if let Err(e) = write_hello(&mut stream, w, h, scale) {
        eprintln!("nixlock: socket: HELLO write failed: {e}");
        return;
    }
    eprintln!("nixlock: socket: client connected, geometry {w}x{h}");

    let reader = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("nixlock: socket: stream clone failed: {e}");
            return;
        }
    };
    *state.active_stream.lock().unwrap() = Some(stream);

    let state = Arc::clone(state);
    std::thread::spawn(move || read_frames(reader, my_gen, state));
}

fn write_hello(stream: &mut UnixStream, width: u32, height: u32, scale: u32) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(8 + 12);
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&width.to_le_bytes());
    buf.extend_from_slice(&height.to_le_bytes());
    buf.extend_from_slice(&scale.to_le_bytes());
    stream.write_all(&buf)
}

fn read_frames(mut stream: UnixStream, my_gen: u64, state: Arc<SocketState>) {
    let mut header = [0u8; 8];
    loop {
        if state.active_gen.load(Ordering::SeqCst) != my_gen {
            break; // superseded by a newer connection; that thread already owns the display
        }
        if let Err(e) = stream.read_exact(&mut header) {
            if e.kind() != ErrorKind::UnexpectedEof {
                eprintln!("nixlock: socket: read frame header failed: {e}");
            }
            break; // clean EOF or a real error -- either way, the client is gone
        }
        let width = u32::from_le_bytes(header[0..4].try_into().unwrap());
        let height = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let expect = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        if expect == 0 || expect > MAX_FRAME_BYTES {
            eprintln!("nixlock: socket: frame {width}x{height} rejected (implausible size); closing");
            break; // no reliable way to resync the byte stream without a valid length
        }
        let mut payload = vec![0u8; expect];
        if let Err(e) = stream.read_exact(&mut payload) {
            eprintln!("nixlock: socket: frame {width}x{height} truncated: {e}; closing");
            break;
        }
        let (ew, eh, _) = *state.geometry.lock().unwrap();
        if width != ew || height != eh {
            eprintln!("nixlock: socket: frame {width}x{height} rejected (kiosk is {ew}x{eh})");
            continue; // FRAME validation: fully read, but dropped -- never blitted, connection kept
        }
        eprintln!("nixlock: socket: frame {width}x{height} accepted");
        *state.latest.lock().unwrap() = Some(LatestFrame { width, height, rgba: payload });
        if let Some(ping) = state.ping.lock().unwrap().as_ref() {
            ping.ping();
        }
    }
    // Only the still-current generation clears the frame back to the default clock -- a
    // superseded connection's late exit must never stomp the NEW connection's already-live frame.
    if state.active_gen.load(Ordering::SeqCst) == my_gen {
        *state.latest.lock().unwrap() = None;
        eprintln!("nixlock: socket: client disconnected; kiosk falls back to the default clock");
    }
}
