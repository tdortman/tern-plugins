//! What changes between the current system and the one nh just built, from
//! `dix --output json`, the library nh prints its own diff with.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::SystemTime,
};

use serde::Deserialize;

/// The arguments of process `pid`.
pub fn cmdline(pid: u32) -> Option<Vec<String>> {
    let raw = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(
        raw.split(|&b| b == 0)
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect(),
    )
}

/// `args[0]` without its directory or a Nix wrapper's `.…-wrapped` name.
pub fn program(args: &[String]) -> Option<&str> {
    Some(
        args.first()?
            .rsplit('/')
            .next()?
            .trim_start_matches('.')
            .trim_end_matches("-wrapped"),
    )
}

/// The two closures nh compares once its build ends.
pub struct Target {
    /// The profile running now.
    old: PathBuf,
    /// The `--out-link` of the build nh pipes into this process.
    out_link: PathBuf,
}

/// When this process's parent is `nh os|home|darwin …`: its current profile,
/// and the out-link of the `nix build` beside this process.
pub fn target(parent: u32) -> Option<Target> {
    let args = cmdline(parent)?;
    if program(&args)? != "nh" {
        return None;
    }
    let old = match args.get(1)?.as_str() {
        "os" | "darwin" => PathBuf::from("/run/current-system"),
        "home" => home_profile()?,
        _ => return None,
    };
    Some(Target {
        old,
        out_link: sibling_out_link(parent)?,
    })
}

/// home-manager's profile, in the new and in the old location.
fn home_profile() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state")))?;
    let user = std::env::var("USER").unwrap_or_default();
    [
        state.join("nix/profiles/home-manager"),
        PathBuf::from(format!(
            "/nix/var/nix/profiles/per-user/{user}/home-manager"
        )),
    ]
    .into_iter()
    .find(|p| p.exists())
}

/// The `--out-link` of the `nix build` that `parent` runs beside this
/// process, made absolute against that process's directory.
fn sibling_out_link(parent: u32) -> Option<PathBuf> {
    for entry in fs::read_dir("/proc").ok()? {
        let Some(pid) = entry
            .ok()
            .and_then(|e| e.file_name().to_str()?.parse::<u32>().ok())
        else {
            continue;
        };
        // `comm` may hold spaces and parentheses, so fields count from the
        // last `)`: state, then the parent's pid.
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let ppid = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(1)?.parse::<u32>().ok());
        if ppid != Some(parent) {
            continue;
        }
        let Some(args) = cmdline(pid) else {
            continue;
        };
        if program(&args) != Some("nix") {
            continue;
        }
        let link = args
            .iter()
            .position(|a| a == "--out-link" || a == "-o")
            .and_then(|i| args.get(i + 1))?;
        let cwd = fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
        return Some(cwd.join(link));
    }
    None
}

impl Target {
    /// The diff, when the build replaced the out-link after `started`; a
    /// failed build leaves an older link (or none) behind.
    pub fn diff(&self, started: SystemTime) -> Option<Closure> {
        let made = fs::symlink_metadata(&self.out_link).ok()?.modified().ok()?;
        if made < started {
            return None;
        }
        let dix = std::env::var_os("TERN_NOM_DIX")?;
        let (old, new) = (
            fs::canonicalize(&self.old).ok()?,
            fs::canonicalize(&self.out_link).ok()?,
        );
        // dix 3 diffs under a `diff` subcommand; dix 2 takes the paths alone.
        let stdout = [&["diff"][..], &[]].into_iter().find_map(|sub| {
            let out = Command::new(&dix)
                .args(["--color", "never"])
                .args(sub)
                .args(["--output", "json"])
                .args([&old, &new])
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()?;
            out.status.success().then_some(out.stdout)
        })?;
        serde_json::from_slice(&stdout).ok()
    }
}

/// `dix --output json`.
#[derive(Debug, Deserialize)]
pub struct Closure {
    pub diffs: Vec<Package>,
    pub paths: Paths,
    pub size_old: u64,
    pub size_new: u64,
}

/// Store path counts.
#[derive(Debug, Deserialize)]
pub struct Paths {
    pub old: u64,
    pub new: u64,
    pub added: u64,
    pub removed: u64,
}

/// One package whose versions or size changed.
#[derive(Debug, Deserialize)]
pub struct Package {
    pub name: String,
    pub versions: Vec<Version>,
    /// `Upgraded`, `Downgraded`, `Changed`, `Mixed`, `Added` or `Removed`.
    pub status: String,
    /// `Selected`, `NewlySelected` or `NewlyUnselected` for packages in the
    /// system's package list, `Unselected` for dependencies.
    pub selection: String,
    pub size_delta: i64,
}

impl Package {
    /// Whether it is in the system's package list (dix's `*`), before or
    /// after.
    pub fn selected(&self) -> bool {
        self.selection != "Unselected"
    }

    /// Its versions before and after, like dix: `1.2, 1.3 ×2`.
    pub fn versions(&self) -> (String, String) {
        let (mut old, mut new) = (Vec::new(), Vec::new());
        for v in &self.versions {
            match v {
                Version::Changed { old: o, new: n } => {
                    old.push(o.label(o.amount));
                    new.push(n.label(n.amount));
                }
                Version::Added { version } => new.push(version.label(version.amount)),
                Version::Removed { version } => old.push(version.label(version.amount)),
                Version::AmountChanged {
                    version,
                    old_amount,
                    new_amount,
                } => {
                    old.push(version.label(Some(*old_amount)));
                    new.push(version.label(Some(*new_amount)));
                }
                Version::Other => {}
            }
        }
        (old.join(", "), new.join(", "))
    }
}

/// How one version changed.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Version {
    Changed {
        old: Named,
        new: Named,
    },
    Added {
        version: Named,
    },
    Removed {
        version: Named,
    },
    AmountChanged {
        version: Named,
        old_amount: u32,
        new_amount: u32,
    },
    /// A kind a newer dix added.
    #[serde(other)]
    Other,
}

/// A version and how many store paths have it.
#[derive(Debug, Deserialize)]
pub struct Named {
    name: String,
    #[serde(default)]
    amount: Option<u32>,
}

impl Named {
    /// `1.2`, or `1.2 ×3` for several paths of it.
    fn label(&self, amount: Option<u32>) -> String {
        match amount {
            Some(n) if n > 1 => format!("{} ×{n}", self.name),
            _ => self.name.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_read_like_dix() {
        let json = r#"{"diffs":[
            {"name":"abseil-cpp","versions":[
                {"kind":"removed","version":{"name":"20260107.1","amount":1}},
                {"kind":"amount_changed","version":{"name":"20260817.0"},"old_amount":3,"new_amount":4}],
             "status":"Downgraded","selection":"Unselected","has_omitted_versions":true,
             "size_old":1,"size_new":2,"size_delta":174744},
            {"name":"tern-nom","versions":[
                {"kind":"changed","old":{"name":"0.6.0","amount":1},"new":{"name":"0.7.0","amount":1}},
                {"kind":"renamed"}],
             "status":"Upgraded","selection":"Selected","has_omitted_versions":false,
             "size_old":1,"size_new":2,"size_delta":-68424}],
            "paths":{"old":4138,"new":4138,"added":24,"removed":24},
            "size_old":62503271784,"size_new":62506034952}"#;
        let closure: Closure = serde_json::from_str(json).expect("dix JSON");
        let [abseil, nom] = &closure.diffs[..] else {
            panic!("two packages");
        };
        assert_eq!(
            abseil.versions(),
            ("20260107.1, 20260817.0 ×3".into(), "20260817.0 ×4".into())
        );
        assert!(!abseil.selected());
        assert_eq!(nom.versions(), ("0.6.0".into(), "0.7.0".into()));
        assert!(nom.selected());
    }
}
