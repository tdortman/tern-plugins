{ pkgs, ... }:
pkgs.mkShell {
  nativeBuildInputs = with pkgs; [
    cargo
    cargo-nextest
    clippy
    rust-analyzer
    rustc
    rustfmt
  ];

  # `cargo run -- build …` falls back to this nom outside Tern. It stays off
  # `PATH`, where it would shadow the installed tern-nom.
  shellHook = ''
    export TERN_NOM_REAL=${pkgs.lib.getExe' pkgs.nix-output-monitor "nom"}
    export TERN_NOM_DIX=${pkgs.lib.getExe pkgs.dix}
  '';
}
