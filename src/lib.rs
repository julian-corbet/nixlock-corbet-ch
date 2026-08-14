//! # nixlock
//!
//! A content-agnostic Wayland session locker that keeps designated **kiosk** outputs live (a
//! pluggable dashboard) while every other output is a PAM lock screen. Full `ext-session-lock`
//! security, split by output role. CPU/`wl_shm` rendered — no GL/Mesa — so it drops into any
//! wlroots compositor.
//!
//! Link nixlock as a library and implement [`KioskContent`] to render your own kiosk output:
//!
//! ```no_run
//! use nixlock::{run, Config, Frame, KioskContent};
//! struct Dashboard;
//! impl KioskContent for Dashboard {
//!     fn paint(&mut self, f: &Frame) -> Vec<u8> { vec![0u8; (f.width * f.height * 4) as usize] }
//! }
//! run(Config { kiosk_outputs: vec!["DP-3".into()], pam_service: "nixlock".into(), username: None },
//!     Dashboard).unwrap();
//! ```
//!
//! The `Session` outputs get the shipped [`ClockLockScreen`] (override it via [`builder`]).

mod auth;
mod content;
mod locker;
mod lockscreen;
mod theme;

pub use content::{AuthState, AuthView, Frame, KioskContent, OutputRole};
pub use locker::{builder, run, Builder, Config, LockError};
pub use lockscreen::ClockLockScreen;
