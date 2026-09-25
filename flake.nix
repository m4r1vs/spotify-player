{
  description = "spotify-player flake";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    nixpkgs,
    flake-utils,
    ...
  }:
    {
      # Builds against the consumer's nixpkgs; tweak features with
      # `spotify-player.override { withAudioBackend = "pulseaudio"; ... }`.
      overlays.default = final: _prev: {
        spotify-player = final.callPackage ./default.nix {};
      };
    }
    // flake-utils.lib.eachDefaultSystem (
      system: let
        pkgs = import nixpkgs {inherit system;};
        spotify-player = pkgs.callPackage ./default.nix {};
      in {
        packages = {
          inherit spotify-player;
          default = spotify-player;
        };
        devShells.default = with pkgs;
          mkShell {
            nativeBuildInputs = [
              pkg-config
              cmake
              autoconf
              automake
              libtool
              rust-analyzer
              rustPlatform.bindgenHook
              cargo
              rustc
            ];
            buildInputs = [
              # spotify-player dependencies
              fontconfig
              libsixel
              openssl
            ] ++ lib.optionals stdenv.hostPlatform.isLinux [
              alsa-lib
              dbus
              dbus-glib
              libpulseaudio
            ];
          };
      }
    );
}
