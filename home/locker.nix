# nixlock on the home-manager plane: put the locker on PATH, render its per-host
# config.json, and wire it in as the session's idle/lock command.
#
# WHY A CONFIG FILE AT ALL, INSTEAD OF CLI FLAGS. The fleet's idle daemon is
# swayidle, and swayidle invokes a locker as a black box: `<lockCommand> -f`,
# nothing more. It has no way to pass this host's kiosk output name, its content
# command, or its PAM service on that command line -- so every per-host VALUE
# has to reach the binary through a channel swayidle does not touch. That
# channel is `$XDG_CONFIG_HOME/nixlock/config.json`, rendered here from the
# options below. `-f` (COMPAT-1) stays the entire CLI surface.
#
# MECHANISM PUBLIC, VALUES PRIVATE. This module is the mechanism: it knows HOW
# to render the config and HOW to hand the lock command to the session. The
# real values -- which physical connector is the kiosk, what dashboard command
# feeds it -- are a host's own business and live in the private infra tree, not
# here. Everything in this file is either a neutral default or an example
# (`DP-3`, `nixwatch-kiosk`); nothing names a real host, output, or command.
{ lib, config, pkgs, ... }:
let
  cfg = config.nixlock;
in
{
  options.nixlock = {
    enable = lib.mkEnableOption "nixlock as this session's screen locker";

    package = lib.mkPackageOption pkgs "nixlock" { };

    kioskOutputs = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "DP-3" ];
      description = ''
        Connector names of the outputs that stay a LIVE dashboard while every
        other output is a lock screen (BEHAVIORS.md KIOSK-1). These are wlroots
        connector names -- `DP-3`, `HDMI-A-1`, `eDP-1` -- the same spelling a
        compositor uses, and a fact about the socket on this host, so they are
        declared per host (a sibling project such as nixdisplay is where those
        names are catalogued; nixlock only consumes the string, with no flake
        input edge). Empty means every output is a lock screen -- a plain
        full-lock with no kiosk role, which is the swaylock-equivalent baseline.
      '';
    };

    kioskCommand = lib.mkOption {
      type = lib.types.nullOr (lib.types.listOf lib.types.str);
      default = null;
      example = [ "nixwatch-kiosk" ];
      description = ''
        PLANNED (out-of-process content path, not wired in v1). An argv that
        produces frames for the kiosk outputs, for a dashboard that is a
        separate program in any language rather than a Rust `KioskContent`
        linked into the binary. In v1 the kiosk content is in-process only, so
        leaving this null is correct; it is rendered into config.json when set
        so a host can declare its intended dashboard ahead of the transport
        landing (see experiments/README.md #003). A failed content command
        falls that output back to the lock screen (KIOSK-2).
      '';
    };

    pamService = lib.mkOption {
      type = lib.types.str;
      default = "nixlock";
      description = ''
        The PAM service the locker authenticates against. Must match a
        `security.pam.services.<name>` on the host -- register it with this
        repo's own `nixosModules.pam` (`nixlock.pam.enable`, whose `service`
        defaults to the same "nixlock"). A name with no matching service fails
        closed, naming itself (BEHAVIORS.md AUTH-3).
      '';
    };

    debug = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Emit credential-blind locker lifecycle and PAM result events to stderr.
        The seated session's systemd user unit captures them in journald. The
        diagnostic stream never includes password content or length, key
        symbols, PAM prompts, or PAM message text. Keep this host-scoped and
        enable it only while diagnosing a locker problem.
      '';
    };

    wireLockCommand = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Whether to set the session's shared idle/lock command to the bare name
        `nixlock`. Default true, which is the fleet path: the session layer
        (nixdesktop) assembles swayidle as `nixlock -f` and re-shows the
        indicator with `pkill -USR1 nixlock`, both of which need the bare
        process name (COMPAT-1). This WRITES `nixdesktop.session.idleAndLock.
        lockCommand`, so it requires that module to be composed into the same
        home-manager evaluation; a consumer who runs nixlock standalone (no
        nixdesktop session layer, wiring swayidle by hand) sets this to false to
        avoid defining an option that then does not exist.
      '';
    };

    lockAfterSeconds = lib.mkOption {
      type = lib.types.nullOr lib.types.ints.positive;
      default = null;
      example = 300;
      description = ''
        Optional: forward an idle-to-lock timeout to the session layer
        (`nixdesktop.session.idleAndLock.lockAfterSeconds`). null -- the default
        -- leaves that module's own default untouched rather than silently
        overriding the host's idle policy from here. Only meaningful when
        `wireLockCommand` is true.
      '';
    };

    suspendAfterSeconds = lib.mkOption {
      type = lib.types.nullOr lib.types.ints.positive;
      default = null;
      example = 600;
      description = ''
        Optional: forward an idle-to-suspend timeout to the session layer
        (`nixdesktop.session.idleAndLock.suspendAfterSeconds`). null leaves that
        module's own default untouched. Only meaningful when `wireLockCommand`
        is true.
      '';
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [
    {
      # The binary on PATH -- this is nixlock's OWN build, resolved through the
      # consumer's overlay, the same way nixnet defaults `package` to its own
      # derivation. swayidle, `pkill`, and a compositor keybind all invoke it by
      # the bare name `nixlock`.
      home.packages = [ cfg.package ];

      # The value channel swayidle cannot reach (see the header). Keys match the
      # default binary's Config: kiosk_outputs, pam_service, and (only when set)
      # kiosk_command. `username` is intentionally omitted -- the binary derives
      # the current user itself, and pinning it here would be a value this
      # module has no business asserting.
      xdg.configFile."nixlock/config.json".text = builtins.toJSON (
        {
          kiosk_outputs = cfg.kioskOutputs;
          pam_service = cfg.pamService;
          debug = cfg.debug;
        }
        // lib.optionalAttrs (cfg.kioskCommand != null) {
          kiosk_command = cfg.kioskCommand;
        }
      );
    }

    # The session wiring, guarded so nixlock does not force nixdesktop on a
    # standalone consumer (see `wireLockCommand`). `mkDefault` so a host that
    # deliberately runs some other locker for one session can still override the
    # command without a conflict.
    (lib.mkIf cfg.wireLockCommand (lib.mkMerge [
      { nixdesktop.session.idleAndLock.lockCommand = lib.mkDefault "nixlock"; }

      # Only touch the timeouts when the consumer actually stated one here --
      # otherwise nixdesktop's own idle policy defaults stand (no silent
      # override of the host's power/lock behaviour from the locker module).
      (lib.mkIf (cfg.lockAfterSeconds != null) {
        nixdesktop.session.idleAndLock.lockAfterSeconds = cfg.lockAfterSeconds;
      })
      (lib.mkIf (cfg.suspendAfterSeconds != null) {
        nixdesktop.session.idleAndLock.suspendAfterSeconds = cfg.suspendAfterSeconds;
      })
    ]))
  ]);
}
