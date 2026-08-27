{
  description = "Content-agnostic Wayland session locker built on ext-session-lock: designated KIOSK outputs stay a live, CPU-rendered dashboard (a pluggable KioskContent) while every other output is a PAM lock screen -- full lock-screen security, split by output role. Ships as a library (implement KioskContent, link it as nixwatch and friends do) plus a default swaylock-CLI-compatible clock-locker binary; CPU/wl_shm render, no GL/Mesa, drops into any wlroots compositor (sway/scroll/hyprland).";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      lib = nixpkgs.lib;
      # x86_64-linux only, and narrow ON PURPOSE. Declaring aarch64 here made CI a lie waiting to
      # happen: `nix flake check` without `--all-systems` silently evaluates only the runner's own
      # system and exits 0, so the extra platform reads as covered while nothing ever built it. And
      # it genuinely cannot be built from a normal runner -- `nix build .#checks.aarch64-linux.
      # eval-checks` on x86_64 fails outright with "platform mismatch", so the strict form would
      # simply be red. No host on this fleet is aarch64 either.
      #
      # Narrow the claim rather than weaken the check: the same call this repo's own
      # `the_socket_module_has_no_route_to_authentication_or_unlock` makes about coverage.
      supportedSystems = [ "x86_64-linux" ];
      forAllSystems = lib.genAttrs supportedSystems;
      pkgsFor = system: import nixpkgs { inherit system; };
    in
    {
      # ---------------------------------------------------------------
      # The lock screen the fleet's swayidle actually invokes. A home-manager
      # module that puts `nixlock` on PATH, renders its per-host config.json
      # from options (which outputs are kiosk, an optional out-of-process
      # content command, the PAM service name), and points the shared idle/lock
      # command at the BARE name `nixlock`. `.default` aliases it because it is
      # the only module in its class and therefore genuinely primary (family
      # contract R6).
      # ---------------------------------------------------------------
      homeManagerModules.locker = ./home/locker.nix;
      homeManagerModules.default = self.homeManagerModules.locker;

      # The one NixOS-plane concern: the `nixlock` PAM service the unprivileged
      # locker authenticates the current user against (unix_chkpwd does the
      # privileged shadow read; see the module's own header). A separate module
      # with no `.default` -- it is a different plane from the home-manager
      # locker and pairs with it by option VALUE (the shared service name),
      # never by a flake input edge (R4).
      nixosModules.pam = ./modules/pam.nix;

      packages = forAllSystems (system: {
        nixlock = (pkgsFor system).callPackage ./package.nix { };
        default = self.packages.${system}.nixlock;
      });

      # Eval-time assertions on the modules' RENDERED values -- the class of bug
      # `nix flake check` never catches on its own, since it only type-checks
      # that a `*Modules` entry IS a module and then never evaluates it. See
      # checks/default.nix for exactly which rendered facts it asserts (the PAM
      # service appears when enabled and is absent when not -- the fail-closed
      # guarantee BEHAVIORS.md AUTH-3 names). The Rust crate's own behaviour is
      # exercised by `cargo test` in-derivation (`nix build .#nixlock`); these
      # are the Nix half: 18 tests over the kiosk display socket's wire protocol and frame
      # acceptance (DISPLAY-1/DISPLAY-2) and over the output-role default (an output nobody
      # configured is a lock screen, never a kiosk). CI runs both halves -- see
      # .github/workflows/ci.yml.
      checks = forAllSystems (system:
        import ./checks {
          pkgs = pkgsFor system;
          nixpkgs = nixpkgs.outPath;
        });

      formatter = forAllSystems (system: (pkgsFor system).nixpkgs-fmt);
    };
}
