//! The default `nixlock` binary: a swaylock-compatible clock locker (the shipped `ClockLockScreen`
//! on every output). Kiosk dashboards come from library consumers (e.g. nixwatch) that link the
//! crate and implement `KioskContent`. Per-host values come from
//! `$XDG_CONFIG_HOME/nixlock/config.json`, because swayidle only ever appends `-f`.

use nixlock::{run, ClockLockScreen, Config};

fn main() {
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

    if let Err(e) = run(load_config(), ClockLockScreen::new()) {
        eprintln!("nixlock: fatal: {e}");
        std::process::exit(1);
    }
}

fn load_config() -> Config {
    #[derive(serde::Deserialize, Default)]
    struct FileCfg {
        #[serde(default)]
        kiosk_outputs: Vec<String>,
        #[serde(default)]
        pam_service: Option<String>,
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
         CONFIG: $XDG_CONFIG_HOME/nixlock/config.json  {{ kiosk_outputs: [..], pam_service: \"..\" }}\n\
         The default binary shows the clock lock screen on every output; kiosk dashboards come from\n\
         library consumers that implement nixlock::KioskContent."
    );
}
