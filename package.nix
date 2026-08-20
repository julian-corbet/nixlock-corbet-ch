# The nixlock build, shared by flake.nix's `packages` output and
# home/locker.nix's `nixlock.package` default so there is exactly one place
# this derivation is defined.
{ lib, rustPlatform, pkg-config, wayland, libxkbcommon, pam }:

rustPlatform.buildRustPackage {
  pname = "nixlock";
  version = "0.1.0";
  src = ./.;

  # Cargo.lock is committed so this builds fully offline and reproducibly --
  # importCargoLock derives its own fixed-output fetch hash straight from the
  # lockfile, no separate vendorHash/cargoHash to keep in sync. Re-run
  # `cargo build` (or `cargo generate-lockfile`) after any dependency bump to
  # refresh it.
  cargoLock.lockFile = ./Cargo.lock;

  # pkg-config locates the three C libraries below. bindgenHook is a member of
  # `rustPlatform` itself, NOT a separate callPackage argument -- pam-sys (via
  # pam-client) runs bindgen against <security/pam_appl.h> at build time, and
  # the hook is what wires up libclang / the right sysroot for that generated
  # FFI. Without it the pam binding fails to build.
  nativeBuildInputs = [ pkg-config rustPlatform.bindgenHook ];

  # The whole runtime link surface: a Wayland client (wayland), an XKB keymap
  # for the lock screen's keyboard (libxkbcommon), and PAM (pam). Rendering is
  # CPU/tiny-skia into a wl_shm buffer, so there is deliberately NO Mesa/GL/
  # Vulkan here -- that absence is a design property, not an omission (see
  # studies/README.md for why it matters on a foreign distro).
  buildInputs = [ wayland libxkbcommon pam ];

  # One crate: a [lib] (the framework -- the KioskContent trait, OutputRole,
  # Config, Locker::run, the shipped ClockLockScreen) plus a default [[bin]]
  # named nixlock (the swaylock-CLI-compatible clock locker). buildRustPackage's
  # default checkPhase runs `cargo test`, so `nix build .#nixlock` IS the test
  # run; only the binary is installed.
  #
  # WHAT THAT COVERS, so the sentence above is not read as more than it is: the
  # kiosk display socket end to end against a real UnixStream -- HELLO wire
  # format, frame accept/drop by geometry, the size cap, truncation, disconnect
  # falling back to the clock, one-client-at-a-time generations, and a
  # source-level assertion that the socket module has no route to PAM or the
  # unlock call (DISPLAY-2) -- plus the output-role default, where anything not
  # named a kiosk output must resolve to a lock screen.
  #
  # WHAT IT DOES NOT: anything needing a live compositor (LOCK-1, LOCK-2,
  # KIOSK-1) or a real PAM stack (AUTH-*, SESSION-1). Those stay manual --
  # `nixlock --check-auth` exists for the PAM half. Every test here was
  # mutation-checked: nine deliberate breaks, nine failures naming themselves.
  meta = {
    description = "Content-agnostic Wayland session locker: designated kiosk outputs stay a live dashboard while every other output is a PAM lock screen -- full ext-session-lock security, split by output role";
    homepage = "https://github.com/julian-corbet/nixlock-corbet-ch";
    license = lib.licenses.mit;
    mainProgram = "nixlock";
    platforms = lib.platforms.linux;
  };
}
