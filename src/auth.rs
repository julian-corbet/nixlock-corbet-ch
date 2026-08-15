//! PAM authentication on a dedicated worker thread, so the blocking PAM stack never stalls the
//! render loop (the dashboard/clock keep ticking during a verify). Fail-closed: only a correct
//! password unlocks; every error path stays locked.
//!
//! Privilege model: nixlock runs entirely as the logged-in user, UNPRIVILEGED. `pam_unix` execs
//! the setuid `unix_chkpwd` to read `/etc/shadow` — that privilege lives in the audited pam helper,
//! never in nixlock. nixlock must never be setuid.

use std::ffi::{CStr, CString};
use std::sync::mpsc;
use zeroize::Zeroizing;

/// Verdict sent worker -> loop. Never carries the password.
pub enum AuthOutcome {
    Unlocked,
    Wrong,
    MaxTries,
    Error,
}

/// A `pam_client::ConversationHandler` that answers PAM's password prompts straight out of a
/// zeroizing buffer and nothing else (`AUTH-1`).
///
/// `pam_client::conv_mock::Conversation` (the crate's built-in mock handler) copies the plaintext
/// into a plain, non-zeroizing `String` internally — fine for its intended use (tests), wrong for a
/// lock screen that holds a real login password. This handler holds exactly one `Zeroizing<String>`
/// and never clones it; the single unavoidable exception is the `CString` each prompt answer
/// returns, because that is the type `libpam`'s C API requires — pam-client hands it across the FFI
/// boundary and it is freed there, outside anything we control. That one conversion is the "one
/// CString libpam requires" the crate's contract makes unavoidable; nothing else is copied.
struct PasswordConversation {
    password: Zeroizing<String>,
}

impl pam_client::ConversationHandler for PasswordConversation {
    /// The normal password prompt (`PAM_PROMPT_ECHO_OFF`, characters not echoed).
    fn prompt_echo_off(&mut self, _prompt: &CStr) -> Result<CString, pam_client::ErrorCode> {
        CString::new(self.password.as_bytes()).map_err(|_| pam_client::ErrorCode::CONV_ERR)
    }

    /// Fallback only: some PAM stacks issue an echoed prompt (`PAM_PROMPT_ECHO_ON`) instead. A
    /// plain `pam_unix` password check never does, but answering it the same way (rather than
    /// refusing) keeps a legitimate, differently-configured stack working without ever showing or
    /// echoing anything ourselves.
    fn prompt_echo_on(&mut self, _prompt: &CStr) -> Result<CString, pam_client::ErrorCode> {
        CString::new(self.password.as_bytes()).map_err(|_| pam_client::ErrorCode::CONV_ERR)
    }

    /// `PAM_TEXT_INFO` — ignored. PAM module chatter never reaches the lock screen or a log
    /// (`AUTH-1`: the only thing this handler ever says back is the password itself, to PAM).
    fn text_info(&mut self, _msg: &CStr) {}

    /// `PAM_ERROR_MSG` — ignored, same reasoning as `text_info`.
    fn error_msg(&mut self, _msg: &CStr) {}

    /// Anything asking a yes/no question is unexpected for a password check — fail closed rather
    /// than guess an answer.
    fn radio_prompt(&mut self, _prompt: &CStr) -> Result<bool, pam_client::ErrorCode> {
        Err(pam_client::ErrorCode::CONV_ERR)
    }

    /// Binary protocol exchange is unexpected here too — fail closed.
    fn binary_prompt(
        &mut self,
        _type: u8,
        _data: &[u8],
    ) -> Result<(u8, Vec<u8>), pam_client::ErrorCode> {
        Err(pam_client::ErrorCode::CONV_ERR)
    }
}

/// One PAM attempt, built + run entirely on the worker thread (pam `Context` is `!Send`).
fn authenticate_once(service: &str, user: &str, password: Zeroizing<String>) -> AuthOutcome {
    use pam_client::{Context, Flag};

    let conv = PasswordConversation { password };
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
    // `ctx` (and the `PasswordConversation` it owns) drops here: the `Zeroizing<String>` wipes
    // itself, and this is the only long-lived copy of the plaintext there ever was.
}

/// Verbose one-shot auth check for `nixlock --check-auth`: runs the exact auth path and prints
/// which PAM stage fails and its error code. Never prints the password (only its length).
pub fn diagnose(service: &str, user: &str, password: String) {
    use pam_client::{Context, Flag};
    eprintln!("nixlock[diag]: service={service:?} user={user:?} pw_len={}", password.len());
    let conv = PasswordConversation {
        password: Zeroizing::new(password),
    };
    let mut ctx = match Context::new(service, Some(user), conv) {
        Ok(c) => {
            eprintln!("nixlock[diag]: Context::new OK");
            c
        }
        Err(e) => {
            eprintln!("nixlock[diag]: Context::new FAILED: {e}");
            return;
        }
    };
    match ctx.authenticate(Flag::NONE) {
        Ok(()) => eprintln!("nixlock[diag]: authenticate() OK"),
        Err(e) => {
            eprintln!("nixlock[diag]: authenticate() FAILED code={:?}: {e}", e.code());
            return;
        }
    }
    match ctx.acct_mgmt(Flag::NONE) {
        Ok(()) => eprintln!("nixlock[diag]: acct_mgmt() OK  =>  WOULD UNLOCK"),
        Err(e) => eprintln!("nixlock[diag]: acct_mgmt() FAILED code={:?}: {e}", e.code()),
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
