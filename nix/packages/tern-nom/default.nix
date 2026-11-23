{
  lib,
  dix,
  inputs,
  makeBinaryWrapper,
  nix-output-monitor,
  rustPlatform,
  ...
}:
let
  src = inputs.self;

in
rustPlatform.buildRustPackage {
  inherit ((fromTOML (builtins.readFile "${src}/crates/nom/Cargo.toml")).package) version;
  inherit src;
  pname = "tern-nom";

  nativeBuildInputs = [
    makeBinaryWrapper
  ];

  cargoLock.lockFile = "${src}/Cargo.lock";

  # Installed as `nom` so it drops in for nix-output-monitor. Everything it
  # doesn't draw itself goes to the real nom.
  postInstall = ''
    mv $out/bin/tern-nom $out/bin/nom
    wrapProgram $out/bin/nom \
      --set TERN_NOM_REAL ${lib.getExe' nix-output-monitor "nom"} \
      --set TERN_NOM_DIX ${lib.getExe dix}
    ln -s ${lib.getExe' nix-output-monitor "nom-build"} $out/bin/nom-build
    ln -s ${lib.getExe' nix-output-monitor "nom-shell"} $out/bin/nom-shell
  '';

  cargoBuildFlags = [
    "-p"
    "tern-nom"
  ];

  cargoTestFlags = [
    "-p"
    "tern-nom"
  ];

  useNextest = true;

  meta = {
    description = "nix-output-monitor drawn natively in Tern";
    license = lib.licenses.mit;
    mainProgram = "nom";
  };
}
