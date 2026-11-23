{
  lib,
  stdenv,
  inputs,
  makeBinaryWrapper,
  nh,
  symlinkJoin,
  ...
}:
let
  nom = lib.getExe' inputs.self.packages.${stdenv.hostPlatform.system}.tern-nom "nom";
in
# Older nh pipes nix into the `nom` on its PATH instead of monitoring builds itself.
assert lib.assertMsg (lib.versionAtLeast nh.version "4.5")
  "nh-tern needs nh 4.5 or newer, not ${nh.version}";
# nh whose build and package diff tern-nom draws in a Tern pane.
symlinkJoin {
  nativeBuildInputs = [ makeBinaryWrapper ];

  # nh runs the `nix` in libexec/tern-nom, which `nom --nh` puts first on its PATH.
  postBuild = ''
    rm $out/bin/nh
    makeWrapper ${nom} $out/libexec/tern-nom/nix --add-flags "--nix $out/libexec/tern-nom"
    makeWrapper ${nom} $out/bin/nh --add-flags "--nh ${lib.getExe nh} $out/libexec/tern-nom"
  '';

  name = "nh-tern-${nh.version}";
  paths = [ nh ];

  meta = nh.meta // {
    mainProgram = "nh";
  };
}
