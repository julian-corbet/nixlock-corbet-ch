//! The default `nixlock` binary: a swaylock-compatible clock locker (the shipped `ClockLockScreen`
//! on every `Session` output). A `Kiosk` output shows whatever a client streams over the kiosk
//! display Unix socket (e.g. `nixwatch-frames`) -- see `nixlock::socket`'s wire protocol -- and
//! the same built-in clock otherwise. Per-host values come from
//! `$XDG_CONFIG_HOME/nixlock/config.json`, because swayidle only ever appends `-f`.

use nixlock::{run, Config};

fn main() {
    if std::env::args().any(|a| a == "--check-auth") {
        return check_auth_mode();
    }
    let mut daemonize = false;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "-f" | "--daemonize" => daemonize = true,
            "-h" | "--help" => return help(),
            "-v" | "--version" => return println!("nixlock {}", env!("CARGO_PKG_VERSION")),
            other => eprintln!("nixlock: ignoring unknown arg {other}"),
        }
    }
    if daemonize {
        // TODO: proper post-lock daemonization (fork after the lock is held, before any thread is
        // spawned). For now we stay foreground; swayidle spawns commands detached, so idle-timeout
        // locking works — only the `before-sleep` guarantee is not yet honored.
        eprintln!("nixlock: -f accepted (before-sleep daemonization is a TODO)");
    }

    if let Err(e) = run(load_config()) {
        eprintln!("nixlock: fatal: {e}");
        std::process::exit(1);
    }
}

/// `nixlock --check-auth`: read a password from stdin and run the exact PAM path verbosely, to
/// verify a host's PAM service authenticates the current user. Never echoes the password.
fn check_auth_mode() {
    use std::io::Read;
    let cfg = load_config();
    let user = std::env::var("USER").unwrap_or_else(|_| "root".to_string());
    let mut pw = String::new();
    std::io::stdin().read_to_string(&mut pw).ok();
    let pw = pw.trim_end_matches(['\n', '\r']).to_string();
    nixlock::check_auth(&cfg.pam_service, &user, pw);
}

fn load_config() -> Config {
    #[derive(serde::Deserialize, Default)]
    struct FileCfg {
        #[serde(default)]
        kiosk_outputs: Vec<String>,
        #[serde(default)]
        pam_service: Option<String>,
        #[serde(default)]
        socket_path: Option<String>,
    }
    let path = std::env::var("XDG_CONFIG_HOME")
        .map(|d| format!("{d}/nixlock/config.json"))
        .unwrap_or_else(|_| {
            format!("{}/.config/nixlock/config.json", std::env::var("HOME").unwrap_or_default())
        });
    let file: FileCfg = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    Config {
        kiosk_outputs: file.kiosk_outputs,
        pam_service: std::env::var("PAM_SERVICE")
            .ok()
            .or(file.pam_service)
            .unwrap_or_else(|| "nixlock".to_string()),
        username: None,
        socket_path: file.socket_path.map(std::path::PathBuf::from),
    }
}

fn help() {
    println!(
        "nixlock — a Wayland session locker that keeps kiosk outputs live while locking the rest.\n\
         \n\
         USAGE: nixlock [-f] [-h] [-v]\n\
           -f, --daemonize   swaylock-compatible flag (fork after lock; TODO)\n\
           -h, --help        this help\n\
           -v, --version     print version\n\
         \n\
         CONFIG: $XDG_CONFIG_HOME/nixlock/config.json\n\
           {{ kiosk_outputs: [..], pam_service: \"..\", socket_path: \"..\" }}\n\
         \n\
         The default binary shows the clock lock screen on every `Session` output. A `Kiosk`\n\
         output shows whatever a client streams over the kiosk display Unix socket (DISPLAY-1) --\n\
         socket_path, default $XDG_RUNTIME_DIR/nixlock.sock -- or the same built-in clock until a\n\
         frame arrives / after the client disconnects. That socket is DISPLAY-ONLY: it can never\n\
         unlock the session (DISPLAY-2); unlock is PAM-only, exactly as on every Session output."
    );
}
