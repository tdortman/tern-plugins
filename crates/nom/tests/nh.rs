//! `nom --nh` and `nom --nix`, run as the `nh` package installs them, against
//! scripts that stand in for nh and nix.

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::Command,
};

/// An empty directory for one test.
fn scratch(test: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

/// Writes the executable shell script `dir/name`.
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    fs::create_dir_all(dir).expect("the script's directory");
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("a script");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("an executable script");
    path
}

/// tern-nom with `path` as its `PATH` and Tern's variables as given.
fn nom(path: &str, tern: &[(&str, &str)]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tern-nom"));
    for name in [
        "TERN_PANE",
        "TERN_TSP",
        "TERM_PROGRAM",
        "TMUX",
        "STY",
        "ZELLIJ",
        "NH_ROM",
    ] {
        cmd.env_remove(name);
    }
    cmd.env("PATH", path).envs(tern.iter().copied());
    cmd
}

#[test]
fn nix_shim_runs_the_next_nix_on_path() {
    let dir = scratch("nix_shim");
    let (shim, real) = (dir.join("shim"), dir.join("real"));
    // Running the shim's own `nix` again would never reach the real one.
    script(&shim, "nix", "echo shim");
    script(&real, "nix", r#"echo "real $* $PATH""#);
    let path = format!("{}:{}", shim.display(), real.display());

    let out = nom(&path, &[])
        .args(["--nix", shim.to_str().expect("a UTF-8 path"), "eval", "x"])
        .output()
        .expect("tern-nom runs");

    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("real eval x {}\n", real.display())
    );
}

#[test]
fn nh_in_a_tern_pane_builds_through_the_shim() {
    let dir = scratch("nh_in_tern");
    let nh = script(&dir, "nh", r#"echo "$* rom=$NH_ROM $PATH""#);

    let out = nom("/bin", &[("TERN_PANE", "1")])
        .args(["--nh", nh.to_str().expect("a UTF-8 path"), "/shim"])
        .args(["os", "switch", "--ask"])
        .output()
        .expect("tern-nom runs");

    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "os switch --diff never --ask rom=0 /shim:/bin\n"
    );
}

#[test]
fn nh_outside_tern_runs_unchanged() {
    let dir = scratch("nh_outside_tern");
    let nh = script(&dir, "nh", r#"echo "$* rom=$NH_ROM $PATH""#);

    let out = nom("/bin", &[])
        .args(["--nh", nh.to_str().expect("a UTF-8 path"), "/shim"])
        .args(["os", "switch"])
        .output()
        .expect("tern-nom runs");

    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "os switch rom= /bin\n"
    );
}
