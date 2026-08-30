# Eval-time checks on nixlock's Nix modules -- the render-time assertions that
# `nix flake check` alone never makes (it confirms a `*Modules` entry IS a
# module, then never evaluates it). Both directions on the one derivation that
# matters on the NixOS plane: enabling `nixlock.pam.enable` MUST register a
# `security.pam.services.nixlock` service, and leaving it off MUST NOT. That is
# the fail-closed guarantee BEHAVIORS.md AUTH-3 depends on -- a lock that opens
# a PAM handle for a service the host never defined must deny, not admit --
# checked here on the build host before any machine loads it.
#
# Only the PAM module is evaluated. The home-manager locker module needs the
# home-manager module system, which this nixpkgs-only flake deliberately does
# not input, so its rendered config.json is asserted by the consuming infra's
# own home evaluation rather than here. Keeping this to a pure nixpkgs NixOS
# eval is exactly what lets `nix flake check` run it with no extra inputs.
{ pkgs, nixpkgs, nixlock }:
let
  lib = pkgs.lib;
  system = pkgs.stdenv.hostPlatform.system;

  # eval-config.nix is nixpkgs' own supported entry point for evaluating a
  # NixOS configuration outside a full system build. We only ever read one
  # option off `config`, so nothing forces a bootloader or fileSystems.
  evalPam = extra: (import (nixpkgs + "/nixos/lib/eval-config.nix") {
    inherit system;
    modules = [ ../modules/pam.nix extra ];
  }).config;

  enabled = evalPam { nixlock.pam.enable = true; };
  disabled = evalPam { };

  hasService = c: c.security.pam.services ? nixlock;

  assertions = [
    { name = "pam-service-present-when-enabled"; ok = hasService enabled; }
    { name = "pam-service-absent-when-disabled"; ok = !(hasService disabled); }
  ];

  failures = lib.filter (a: !a.ok) assertions;
in
{
  eval-checks = pkgs.runCommand "nixlock-eval-checks" { }
    (if failures == [ ]
    then "echo 'nixlock module eval checks passed (${toString (lib.length assertions)})' > $out"
    else throw "nixlock eval checks failed: ${toString (map (a: a.name) failures)}");

  # LOCK-3 against a real, private compositor. The fixture starts Sway with zero outputs, waits
  # until nixlock owns the session lock, then adds a headless output through a PID-bound IPC socket
  # inside its private XDG_RUNTIME_DIR. The seated compositor is never in scope.
  headless-output-hotplug = pkgs.runCommand "nixlock-headless-output-hotplug" {
    nativeBuildInputs = [ pkgs.bash pkgs.coreutils pkgs.gnugrep pkgs.sway nixlock ];
  } ''
    bash ${./headless-output-hotplug.sh} \
      ${nixlock}/bin/nixlock \
      ${pkgs.sway}/bin/sway \
      ${pkgs.sway}/bin/swaymsg
    touch "$out"
  '';
}
