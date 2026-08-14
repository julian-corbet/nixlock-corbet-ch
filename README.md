# nixlock

A content-agnostic Wayland session locker that keeps designated **kiosk**
outputs live -- a real, updating dashboard -- while every other output is a PAM
lock screen. Full `ext-session-lock` security, split by output role: the
private screens prompt for a password, the kiosk screen never does.

That split is the novel part. Prior art (swaylock-plugin, hyprlock) can draw
something animated behind the password field, but every output is still the same
lock with the same prompt. nixlock adds a **kiosk role**: an output that stays a
live display with *no password affordance and no input at all*, next to other
outputs that are ordinary lockers -- so a wall-mounted status panel keeps showing
the clock, the queue, the graphs, while the machine it is attached to is
genuinely locked.

It is built on smithay-client-toolkit 0.20 and renders on the **CPU** into
`wl_shm` buffers (tiny-skia, no GL/Vulkan/Mesa), so it drops into any wlroots
compositor -- sway, scroll, hyprland -- without a driver in its closure.

nixlock ships two things from one crate:

- a **library** -- implement the `KioskContent` trait with your dashboard and
  link it (a consumer such as `nixwatch` does exactly this); and
- a **default binary**, `nixlock`, a swaylock-CLI-compatible clock locker you
  can drop in with zero code.

## Quickstart

Home-manager module (the fleet path). Add the flake, import the locker module,
turn it on:

```nix
{
  inputs.nixlock.url = "github:julian-corbet/nixlock-corbet-ch";
}
# home configuration:
imports = [ inputs.nixlock.homeManagerModules.default ];

nixlock = {
  enable = true;
  kioskOutputs = [ "DP-3" ];        # this screen stays a live dashboard
  pamService   = "nixlock";         # matches the NixOS module below
};
```

and the matching PAM service on the NixOS side (see
[PAM & keyboard](#pam--keyboard)):

```nix
imports = [ inputs.nixlock.nixosModules.pam ];
nixlock.pam.enable = true;          # registers security.pam.services.nixlock
```

The home module puts `nixlock` on PATH, renders
`$XDG_CONFIG_HOME/nixlock/config.json` from those options, and points the
session's idle/lock command at the bare name `nixlock` -- so swayidle locks with
`nixlock -f` and nothing else changes. Per-host values live in
`config.json` because swayidle only ever appends `-f` (see
[swaylock compatibility](#swaylock-compatibility)).

## Output roles

Every output the compositor lists at lock time is one of two roles:

- **Session** -- an ordinary lock screen. It prompts for a password; a correct
  PAM password unlocks the whole session (there is one lock, shared across all
  outputs -- unlocking any Session screen unlocks the machine).
- **Kiosk** -- a live display with **no password field and no input**. It renders
  content (a dashboard, or the shipped clock) and keeps updating while the
  machine is locked. Keyboard and pointer over a kiosk output are consumed by
  the lock and never reach the content or any session client
  (`BEHAVIORS.md` KIOSK-1).

Which outputs are kiosk is a per-host value (`kioskOutputs`, a list of connector
names such as `DP-3`). Anything not named is a Session output. Name nothing and
every output is a Session lock -- the plain swaylock-equivalent baseline.

If a kiosk output's content fails, that output falls back to the Session lock
screen (KIOSK-2). A broken dashboard degrades toward *more* lock, never toward a
revealed or blank screen.

## Writing content

Kiosk (and Session) content is a trait. A consumer implements it and links
nixlock as a library:

```rust
use nixlock::{run, Config, Frame, KioskContent};

struct Dashboard { /* your state */ }

impl KioskContent for Dashboard {
    /// Return one frame as premultiplied RGBA, exactly width * height * 4 bytes.
    fn paint(&mut self, frame: &Frame) -> Vec<u8> {
        // draw with tiny-skia (or anything that yields premultiplied RGBA),
        // sized to frame.width x frame.height
        vec![0u8; (frame.width * frame.height * 4) as usize]
    }
    // optional: how often to repaint absent input (default 1s)
    // fn tick_interval(&self) -> std::time::Duration { .. }
}

fn main() {
    let config = Config {
        kiosk_outputs: vec!["DP-3".into()],
        pam_service: "nixlock".into(),
        username: None,               // derive the current user
    };
    run(config, Dashboard { /* .. */ }).unwrap();
}
```

`paint` returns premultiplied RGBA; the framework owns the BGRA swizzle and the
`wl_shm` commit, and rejects a wrong-sized buffer (that output falls back to the
lock screen, KIOSK-2). The auth conversation runs off the render loop, so `paint`
is called on a steady frame clock and never blocks on PAM, and PAM never blocks
the clock (AUTH-2).

An **out-of-process** content path -- a dashboard as a separate program in any
language, feeding frames over a pipe or shared memory -- is **planned, not in
v1** (the `kioskCommand` option and its transport; see
[`experiments/README.md`](experiments/README.md) #003). v1 is the in-process
trait plus the default clock binary.

## The lock screen

The shipped default Session content is `ClockLockScreen`: a centered clock, a
password field, and a failed-attempt indicator -- what the `nixlock` binary uses
out of the box, and a swaylock-shaped experience with no configuration. It is
just an implementation of the same content trait, so a consumer that wants a
different Session look overrides it with `builder()` (a `Builder` that swaps the
default Session content) rather than `run()`; nixlock does not hard-code its own
lock screen into the locking machinery.

## PAM & keyboard

nixlock is **unprivileged** -- an ordinary Wayland client, no setuid bit, no
capabilities. It cannot read `/etc/shadow` and never tries to. Authentication
goes through a **dedicated `nixlock` PAM service** (a plain `pam_unix`
auth+account stack); the one privileged step, reading the shadow hash, is done
by the setuid `unix_chkpwd` helper pam_unix execs -- the same helper every other
PAM consumer on the host uses. The `nixosModules.pam` module registers that
service:

```nix
nixlock.pam.enable = true;   # security.pam.services.nixlock, unix auth+account
```

A dedicated service (rather than borrowing `login` or `swaylock`) is what makes
the lock's auth policy declarable and auditable. It is also **fail-closed**: a
`pamService` naming a service the host never defined makes every attempt fail,
naming the missing service, and never falls back to another one (AUTH-3). The
typed password lives only in a zeroizing buffer and is never logged, stored, or
put in an error (AUTH-1). Keyboard input is decoded through libxkbcommon.

## swaylock compatibility

The default binary is a drop-in for the contract swayidle already speaks:

- `nixlock -f` acquires the lock **first**, then daemonizes -- so nothing races
  ahead of a held lock (COMPAT-1).
- the process stays named `nixlock`, so `pkill -USR1 nixlock` reaches it, and
  `SIGUSR1` re-shows the password indicator (swaylock's own convention).
- per-host values swayidle cannot pass on the command line -- kiosk outputs, the
  kiosk command, the PAM service -- are read from
  `$XDG_CONFIG_HOME/nixlock/config.json`, which the home-manager module renders.

That is the whole CLI surface. nixlock replaces `swaylock` in an existing
swayidle setup with no change to swayidle.

## Non-goals

- **Not a compositor.** The lock is enforced by the compositor via
  `ext-session-lock`; nixlock is the client. No protocol, no fallback overlay --
  if the compositor does not implement `ext-session-lock`, nixlock refuses to run
  rather than draw a bypassable "lock" (LOCK-1).
- **Not an idle daemon.** swayidle (or hypridle) owns idle timeouts, suspend,
  and before-sleep locking. nixlock is only ever the thing they invoke.
- **Not a bar / status surface** for the unlocked desktop. The kiosk role is a
  display *while locked*; a running session's bar is a different program.
- **Not an output/display-layout manager.** nixlock reads output connector names;
  it does not arrange, scale, or enable/disable them.
- **Not multi-factor.** A screen lock here is a low-friction single-factor gate
  by design.

## Behaviour contract

Security invariants are stated as named, outside-observable behaviours in
[`BEHAVIORS.md`](BEHAVIORS.md) (`LOCK-*`, `KIOSK-*`, `AUTH-*`, `SESSION-*`,
`COMPAT-*`). Review that file instead of the code when the question is "what is
this allowed to do."

## Status

Early. v1 is the in-process `KioskContent` trait plus the default clock binary;
the out-of-process content path is planned (see the ledger). Defaults that are
reasoned rather than measured are tracked openly in
[`experiments/README.md`](experiments/README.md), and measured findings in
[`studies/README.md`](studies/README.md).

## Related projects

nixlock is one of several small, independently-usable open-source projects
sharing a common design system: nixnet (transport failover), nixram (RAM-pressure
tuning), nixarch (declarative Arch/CachyOS), and others. nixlock's own niche is
purely the locker; a dashboard project links it as a library, and any wlroots
compositor can use its default binary standalone.

## License

MIT.
