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

nixlock is a **content-blind display SERVER**: the kiosk output shows whatever
premultiplied-RGBA frames a client streams to it over a tiny Unix socket
protocol (see [Streaming kiosk content](#streaming-kiosk-content) below) -- any
language, any process, started and supervised entirely independently of
nixlock. `nixwatch` is exactly such a client (its `nixwatch-frames` binary).
nixlock ships two things from one crate:

- the **socket server** itself (always on, part of every run -- see below); and
- a **default binary**, `nixlock`, a swaylock-CLI-compatible clock locker you
  can drop in with zero code and zero streaming client at all.

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
- **Kiosk** -- a live display with **no password field and no input**. It shows
  whatever a socket client is streaming (a dashboard, or nixlock's own clock
  while nothing is) and keeps updating while the machine is locked. Keyboard and
  pointer over a kiosk output are consumed by the lock and never reach a socket
  client or any session client (`BEHAVIORS.md` KIOSK-1).

Which outputs are kiosk is a per-host value (`kioskOutputs`, a list of connector
names such as `DP-3`). Anything not named is a Session output. Name nothing and
every output is a Session lock -- the plain swaylock-equivalent baseline.

If a kiosk output's socket client sends nothing usable (not connected yet, a
mismatched frame, a disconnect), that output falls back to nixlock's own built-in
clock (`DISPLAY-1`). A broken/absent dashboard degrades toward *more* lock, never
toward a revealed or blank screen.

## Streaming kiosk content

A kiosk output is fed by ANY process that connects to nixlock's Unix socket and
speaks this wire protocol -- not a linked Rust trait, not something that runs
inside nixlock at all. `nixwatch`'s `nixwatch-frames` binary is exactly such a
client; write your own in any language that can open a Unix socket.

**Socket.** `$XDG_RUNTIME_DIR/nixlock.sock` by default (override with
`socket_path` in `config.json`). After the compositor confirms that this process owns the session
lock, nixlock creates and listens on the socket, unlinking any stale path left by a previous run
first. A duplicate locker rejected by the compositor never touches the active locker's socket.
One client at a time -- a new connection replaces whatever was streaming before it.

**On connect**, nixlock immediately writes a **HELLO** (all integers
little-endian):

| bytes | field | meaning |
|---|---|---|
| 8 | magic | `b"NIXLOCK1"` |
| 4 | `width: u32` | the kiosk output's current width in pixels |
| 4 | `height: u32` | the kiosk output's current height in pixels |
| 4 | `scale: u32` | the kiosk output's scale factor |

`width = height = 0` means "no kiosk output" -- either `kioskOutputs` is empty on
this host, or (a narrow startup race) the compositor has not sent the kiosk
surface's first `configure` yet. A client that reads `0x0` should idle or exit
rather than try to stream at that size.

**Then, repeatedly, the client sends FRAMES** — no reply, no acknowledgement:

| bytes | field | meaning |
|---|---|---|
| 4 | `width: u32` | this frame's width |
| 4 | `height: u32` | this frame's height |
| `width*height*4` | pixels | premultiplied RGBA8, row-major, top-down, stride = `width*4` |

nixlock blits the **latest fully-received frame whose geometry matches the kiosk
output exactly** onto it (RGBA -> BGRA swizzle into the `wl_shm` surface, same
path the built-in clock uses). A frame with the wrong geometry is dropped --
never blitted -- and the connection stays open, so a client mid-resize just has
frames ignored until it catches up. Until any valid frame has arrived, or once
the client disconnects, the kiosk output shows nixlock's own clock (`DISPLAY-1`).

**Display-only, always** (`DISPLAY-2`): nixlock parses only the width/height
header on this socket -- it is content-blind to the pixels -- and nothing that
arrives on it can reach the password buffer, the auth state machine, or the
unlock call. Unlock stays PAM-only, exactly as on every Session output. A
reverse channel (input events flowing back to a streamed dashboard) is a
plausible future addition and is deliberately not built here.

`KioskContent` (the trait) still exists, but only as the mechanism BEHIND
nixlock's own built-in clock and the `Session` lock screen -- a library consumer
overrides the latter via `builder().session(..)` (below); it is no longer a
per-output kiosk content API.

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

## Debugging an unlock

Set `nixlock.debug = true` in the home-manager module, put `"debug": true` in
`$XDG_CONFIG_HOME/nixlock/config.json`, or invoke `nixlock --debug`. Debug mode writes structured
events to stderr, which the seated session's systemd user unit captures in journald. It records the
lock lifecycle, output configuration and keyboard focus, an opaque attempt number, PAM stage/result
code and duration, the UI failure/backoff transition, and the compositor unlock request/flush.

It never records password content or length, keystrokes/key symbols, PAM prompts, or PAM message
text. On a systemd user session, inspect the current boot with:

```bash
journalctl -b _SYSTEMD_USER_UNIT=idle.service --grep='nixlock' \
  -o short-monotonic --no-pager
```

The worker emits `pam_finished` before the Wayland loop receives `auth_result_received`. If the
former exists without the latter, the event loop stalled; if both report `unlocked` but
`unlock_flushed` reports an error, the Wayland connection failed; a named PAM code distinguishes a
wrong password from a missing/unavailable auth service without exposing credential material.

## swaylock compatibility

The default binary is a drop-in for the contract swayidle already speaks:

- `nixlock -f` forks before creating any worker thread. The parent returns only
  after the compositor confirms that the child holds the session lock, so
  swayidle's `before-sleep` ordering is real; startup failures propagate as a
  failed hook instead of reporting a lock that was never acquired. See COMPAT-1.
- the process stays named `nixlock`, so `pkill -USR1 nixlock` reaches it, and
  `SIGUSR1` is handled: it is never fatal, and it repaints -- re-showing the
  password indicator -- within one tick (<=1s). It touches no auth state.
- per-host values swayidle cannot pass on the command line -- kiosk outputs, the
  PAM service, the kiosk socket path -- are read from
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
[`BEHAVIORS.md`](BEHAVIORS.md) (`LOCK-*`, `KIOSK-*`, `DISPLAY-*`, `AUTH-*`,
`SESSION-*`, `COMPAT-*`). Review that file instead of the code when the question
is "what is this allowed to do."

## Status

Early. Kiosk content is out-of-process, over the socket protocol above
([Streaming kiosk content](#streaming-kiosk-content)); the default clock binary
needs no client at all. Defaults that are reasoned rather than measured are
tracked openly in
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
