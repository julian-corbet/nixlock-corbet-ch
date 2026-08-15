# Experiments

Throwaway trials: spikes, one-off scripts, measurements not yet worth writing up
properly. Nothing here is guaranteed to work, be maintained, or survive the next
cleanup pass. If something in here turns out to matter, distill the actual
finding into [`../studies/`](../studies/README.md) and let the experiment stay
disposable (or delete it).

This is also the **open-questions ledger** for nixlock's own judgment calls --
every entry below corresponds to a default or design choice that is *reasoned,
not measured*. Nothing here has been measured yet; each entry says what would
settle it. Results feed back into the module defaults, the `Config`/trait shape,
or `BEHAVIORS.md` as they close.

## 001 — dedicated `nixlock` PAM service vs reusing the distro's `login`/`swaylock`

**Question:** the modules register a dedicated `security.pam.services.nixlock`
(NixOS) and the binary defaults `pam_service` to `"nixlock"`. The alternative is
to authenticate against a service the host already ships -- `login`, or
`swaylock`'s own `/etc/pam.d/swaylock`. Which is the least-surprising, safest
default across the two provisioning planes?

**Hypothesis:** a dedicated service is right, because it makes the lock's auth
policy declarable and auditable (a host operator reads exactly what the lock
uses) and fails closed by name (`AUTH-3`) instead of silently inheriting whatever
`login` was configured for. But it has a real cost that differs by plane: on
NixOS it is one declarative `security.pam.services.nixlock` line; on Arch/CachyOS
(system-manager + home-manager) it is an `/etc/pam.d/nixlock` FILE that something
has to place, and a locker whose PAM file is missing is worse than one that
reused a file guaranteed to exist. The open question is whether the default
should stay "dedicated everywhere" or "dedicated on NixOS, reuse `login` where
provisioning the file is out of band."

**Status:** open.

## 002 — `Vec<u8>` per-frame allocation vs a zero-copy paint-into path

**Question:** `KioskContent::paint(&mut self, frame: &Frame) -> Vec<u8>` returns
a freshly allocated premultiplied-RGBA buffer every frame. For a CPU/tiny-skia
renderer at dashboard sizes and tick rates, does allocating a full framebuffer
each tick cost anything worth an API change -- handing the content a mutable
buffer (`&mut [u8]`, or a tiny-skia `Pixmap` it paints into and nixlock copies to
`wl_shm`) instead?

**Hypothesis:** at 1 Hz on one 4K output the allocation is noise; the question is
whether a busier dashboard (many small updates, several outputs) makes it
matter, and whether the ergonomic cost of a paint-into API is worth paying before
any measurement says the allocation shows up. Returning `Vec<u8>` is the simplest
trait to implement against, which is why v1 ships it.

**Method sketch:** a synthetic `KioskContent` at a few sizes/tick rates,
allocation-per-frame vs paint-into, measuring frame-time and allocator pressure.

**Status:** open.

## 003 — out-of-process frame transport for a language-agnostic content plane (stdout pipe vs shm)

**Question:** v1 kiosk content was in-process (implement the trait, link nixlock).
An out-of-process path would let a dashboard in any language feed frames.
Length-prefixed RGBA frames on a child's stdout (dead simple, one copy per frame,
trivial to write a producer for) or a shared-memory ring (zero-copy into the
`wl_shm` buffer, but lifecycle, sizing on output-mode change, and
cleanup-on-crash to get right)?

**Resolution:** neither, exactly -- a third shape turned out to fit better than
either original candidate. nixlock now runs a **Unix-socket server**
(`$XDG_RUNTIME_DIR/nixlock.sock`, see README's "Streaming kiosk content") that
any independently-started, independently-supervised process can dial into and
stream length-prefixed premultiplied-RGBA frames over -- `nixwatch-frames` is
the first such client. This beats a child-process stdout pipe (nixlock does not
spawn or own the content process's lifecycle at all, so a dashboard restarts on
its own systemd unit, on its own schedule, with its own crash/backoff policy --
`DISPLAY-1` covers the "nothing connected / nothing valid yet" case exactly the
way `KIOSK-2` already covered a dead child) and keeps the one-copy-per-frame
simplicity a shared-memory ring would have traded away for a harder lifecycle.
The in-process `KioskContent`-per-output API this entry originally weighed
against is gone; the trait survives only as the mechanism behind the built-in
clock and the `Session` screen (`builder().session(..)`).

**Note:** the home-manager `nixlock.kioskCommand` option (`home/locker.nix`)
describes an OLDER, different planned shape -- nixlock itself spawning and
supervising a content command -- that predates and was superseded by the design
above (nixlock never spawns the streaming client; a host's session config does,
as an ordinary service). It was never wired to anything (always rendered `null`
unless set), so nothing regresses by its continued presence, but it is stale
documentation now and worth retiring in a follow-up pass.

**Status:** resolved -- see README's "Streaming kiosk content" and
`BEHAVIORS.md`'s `DISPLAY-1`/`DISPLAY-2`.

## 004 — default tick / frame interval

**Question:** what cadence should the default clock, and the kiosk frame clock,
run at? 1 Hz is plenty for a clock; a dashboard may want faster. One global
default, or per-content (the trait declares its own desired interval)?

**Hypothesis:** a single conservative default (≈1 Hz) with a per-content override
is likely right -- most kiosk content is glanceable, not animated -- but nothing
has been measured against a real dashboard, and a too-slow default reads as a
frozen panel while a too-fast one wastes CPU on a locked machine.

**Status:** open.

## 005 — password backoff policy: nixlock's own, or PAM's

**Question:** after repeated wrong passwords, does nixlock impose its own delay,
or rely entirely on PAM's (pam_unix's built-in fail delay, `pam_faildelay`)?

**Hypothesis:** rely on PAM. A lock screen that adds its own backoff duplicates
policy PAM already owns and can diverge from every other auth path on the host; a
lock that adds none inherits exactly what the service configures, which is the
declarable, auditable behaviour the dedicated-service choice (#001) is chosen
for. But an unresponsive-feeling delay with no on-screen feedback is its own
usability failure, so the open part is what nixlock SHOWS during a PAM-imposed
delay, not whether it adds one. `SESSION-1`'s "retryable, never a lockout" holds
either way.

**Status:** open.
