//! PAM authentication on a dedicated worker thread, so the blocking PAM stack never stalls the
//! render loop (the dashboard/clock keep ticking during a verify). Fail-closed: only a correct
//! password unlocks; every error path stays locked.
//!
//! Privilege model: nixlock runs entirely as the logged-in user, UNPRIVILEGED. `pam_unix` execs
//! the setuid `unix_chkpwd` to read `/etc/shadow` — that privilege lives in the audited pam helper,
//! never in nixlock. nixlock must never be setuid.

use std::sync::mpsc;
use zeroize::Zeroizing;

/// Verdict sent worker -> loop. Never carries the password.
pub enum AuthOutcome {
    Unlocked,
    Wrong,
    MaxTries,
    Error,
}

/// One PAM attempt, built + run entirely on the worker thread (pam `Context` is `!Send`).
fn authenticate_once(service: &str, user: &str, password: Zeroizing<String>) -> AuthOutcome {
    use pam_client::conv_mock::Conversation;
    use pam_client::{Context, Flag};

    // NOTE: pam-client's mock conversation copies the plaintext; a custom zeroizing
    // ConversationHandler is a tracked hardening (experiments/). The buffer we hold is wiped on drop.
    let conv = Conversation::with_credentials(user.to_owned(), password.to_string());
    let mut ctx = match Context::new(service, Some(user), conv) {
        Ok(c) => c,
        Err(_) => return AuthOutcome::Error, // service missing / init failed -> FAIL CLOSED (AUTH-3)
    };
    match ctx.authenticate(Flag::NONE) {
        Ok(()) => match ctx.acct_mgmt(Flag::NONE) {
            Ok(()) => AuthOutcome::Unlocked, // the ONLY unlock path (SESSION-1)
            Err(_) => AuthOutcome::Error,    // expired/locked account -> stay locked
        },
        Err(e) => {
            use pam_client::ErrorCode::*;
            match e.code() {
                MAXTRIES => AuthOutcome::MaxTries,
                AUTH_ERR | USER_UNKNOWN | CRED_INSUFFICIENT | AUTHINFO_UNAVAIL => AuthOutcome::Wrong,
                _ => AuthOutcome::Error, // abort / conversation error -> FAIL CLOSED
            }
        }
    }
}

/// Spawns the persistent auth worker. Keeping it persistent lets `pam_faillock` counters and
/// backoff accumulate coherently across attempts. Passwords arrive on `attempts`; verdicts go out
/// on `results` (a calloop channel, which wakes the event loop cleanly).
pub fn spawn(
    service: String,
    user: String,
    attempts: mpsc::Receiver<Zeroizing<String>>,
    results: calloop::channel::Sender<AuthOutcome>,
) {
    std::thread::spawn(move || {
        while let Ok(pw) = attempts.recv() {
            let outcome = authenticate_once(&service, &user, pw);
            let done = matches!(outcome, AuthOutcome::Unlocked);
            let _ = results.send(outcome);
            if done {
                break;
            }
        }
    });
}
