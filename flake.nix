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
    flake-utils.lib.eachDefaultSystem (
      system: let
        pkgs = import nixpkgs {inherit system;};
      in {
        defaultPackage = pkgs.callPackage ./default.nix {};
        devShell = with pkgs;
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
              alsa-lib
              dbus
              fontconfig
              dbus-glib
              libsixel
              openssl
              libpulseaudio
            ];
          };
      }
    );
}
