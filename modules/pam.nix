# nixlock's PAM service, on the NixOS plane.
#
# THE PRIVILEGE MODEL, in one paragraph. nixlock runs entirely UNPRIVILEGED: it
# is an ordinary Wayland client with no setuid bit and no capabilities. It
# cannot read /etc/shadow and never tries to. Authentication goes through PAM's
# pam_unix auth+account stack, and the ONE privileged step -- reading the shadow
# hash to verify a password -- is delegated to `unix_chkpwd`, the small setuid
# helper pam_unix execs for exactly this and which every other PAM consumer on
# the host already relies on. So the code that runs as root is unix_chkpwd
# (audited, shared, tiny), never the locker painting pixels next to an
# unlocked-looking screen. Giving nixlock its OWN named PAM service rather than
# borrowing `login` or `swaylock` is what makes that boundary declarable and
# auditable: the service a lock screen authenticates against is named after the
# lock screen, and a host operator can read exactly what policy the lock uses.
#
# This is the whole NixOS-plane surface of nixlock. Everything else the fleet
# needs -- the binary on PATH, the per-host config.json, the idle/lock command
# wiring -- is the home-manager module (home/locker.nix); the two pair by the
# shared service NAME (`service` here, `pamService` there, both defaulting to
# "nixlock"), never by a flake input.
{ lib, config, ... }:
let
  cfg = config.nixlock.pam;
in
{
  options.nixlock.pam = {
    enable = lib.mkEnableOption ''
      the `nixlock` PAM service -- a plain unix auth+account stack the
      unprivileged locker authenticates the current user against
    '';

    service = lib.mkOption {
      type = lib.types.str;
      default = "nixlock";
      description = ''
        The PAM service name to register under `security.pam.services`. It MUST
        match the home-manager locker's `nixlock.pamService` (whose own default
        is also "nixlock") and the default binary's `pam_service` config key: a
        locker that opens a PAM handle for a service name no
        `security.pam.services.<name>` defines fails closed, naming the missing
        service (BEHAVIORS.md AUTH-3) -- it never silently falls back to
        `login` or `other`. Override only to run more than one
        differently-configured nixlock service on a single host.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    # A default unix auth + account service, nothing more. pam_unix does the
    # password check via the setuid unix_chkpwd helper (see the header); the
    # NixOS pam-service defaults already give auth and account through
    # pam_unix, which is exactly and only what a screen lock needs.
    #
    #   - unixAuth = true  -- the password IS the unix password (the point).
    #   - startSession = false -- unlocking a screen RESUMES a session that
    #     already holds its credentials; it does not establish a new login
    #     session, so there is no session/setcred stack to run.
    #
    # Deliberately no password module (a lock screen never CHANGES a password)
    # and deliberately no second factor: this estate designs low-friction
    # single-factor gates on its own services, and a screen lock is the
    # canonical one.
    security.pam.services.${cfg.service} = {
      unixAuth = true;
      startSession = false;
    };
  };
}
