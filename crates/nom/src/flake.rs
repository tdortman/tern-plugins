//! `nix flake`: lock file changes, and the JSON of `metadata` and `show`.

use serde_json::{Map, Value};
use tern_sdk::ui::{self, TreeItem};

use crate::view::Node;

/// One input a lock file update added, updated or removed.
#[derive(Debug, PartialEq, Eq)]
pub struct LockChange {
    /// `Added`, `Updated` or `Removed`.
    pub verb: String,
    /// The input's path, like `nixpkgs` or `snowfall-lib/treefmt-nix`.
    pub input: String,
    /// What it was locked to.
    pub from: Option<Lock>,
    /// What it is locked to now.
    pub to: Option<Lock>,
}

/// What an input is locked to, as nix lists it in a lock file update.
#[derive(Debug, PartialEq, Eq)]
pub enum Lock {
    /// A source at a revision: `github:NixOS/nixpkgs`, `02c087f`, and the
    /// date of that revision.
    Pinned {
        source: String,
        rev: Option<String>,
        date: Option<String>,
    },
    /// The same input as another one, by its path.
    Follows(String),
}

impl Lock {
    /// `'github:NixOS/nixpkgs/<rev>' (2026-10-20)` or `follows 'nixpkgs'`.
    fn parse(line: &str) -> Self {
        let line = line.replace('\'', "");
        if let Some(target) = line.strip_prefix("follows ") {
            return Self::Follows(target.trim().to_owned());
        }
        let (url, date) = match line.rsplit_once(" (") {
            Some((url, date)) => (url, date.strip_suffix(')')),
            None => (line.as_str(), None),
        };
        let (base, query) = url.split_once('?').unwrap_or((url, ""));
        let hash = |s: &str| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit());
        let mut rev = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("rev="))
            .map(str::to_owned);
        // `github:owner/repo/<rev>` and the like carry it in the path.
        let source = match base.rsplit_once('/') {
            Some((repo, last)) if hash(last) => {
                rev = Some(last.to_owned());
                repo
            }
            _ => base,
        };
        Self::Pinned {
            source: source.to_owned(),
            rev: rev.map(|r| r.chars().take(7).collect()),
            // `path:` inputs have no history: nix dates them to the epoch.
            date: date.filter(|d| *d != "1970-01-01").map(str::to_owned),
        }
    }
}

/// The inputs an "updating lock file" or "creating lock file" warning
/// lists; `None` for any other message.
pub fn lock_changes(plain: &str) -> Option<Vec<LockChange>> {
    let mut lines = plain.lines();
    let head = lines.next()?.trim();
    if !(head.starts_with("warning: updating lock file")
        || head.starts_with("warning: creating lock file"))
    {
        return None;
    }
    let mut changes: Vec<LockChange> = Vec::new();
    for line in lines {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("• ") {
            let Some((verb, input)) = rest.split_once(" input ") else {
                continue;
            };
            changes.push(LockChange {
                verb: verb.to_owned(),
                input: input.trim_end_matches(':').trim_matches('\'').to_owned(),
                from: None,
                to: None,
            });
        } else if let Some(change) = changes.last_mut() {
            if let Some(to) = line.strip_prefix("→ ") {
                change.to = Some(Lock::parse(to));
            } else if change.verb == "Updated" {
                change.from = Some(Lock::parse(line));
            } else {
                change.to = Some(Lock::parse(line));
            }
        }
    }
    Some(changes)
}

/// `secs` since the epoch as a UTC `YYYY-MM-DD HH:MM`.
fn date(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60
    )
}

/// What `nix flake metadata --json` or `nix flake show --json` printed.
pub enum Output {
    Metadata(Value),
    Show(Value),
}

impl Output {
    /// The subcommands whose JSON the card draws.
    pub fn drawn(sub: &str) -> bool {
        matches!(sub, "metadata" | "info" | "show")
    }

    /// `json` as printed by `nix flake <sub> --json`.
    pub fn parse(sub: &str, json: &[u8]) -> Option<Self> {
        let value = serde_json::from_slice(json).ok()?;
        Some(if sub == "show" {
            Self::Show(value)
        } else {
            Self::Metadata(value)
        })
    }

    /// The card's nodes for it.
    pub fn view(&self) -> Vec<Node> {
        match self {
            Self::Metadata(v) => metadata(v),
            Self::Show(v) => show(v).into_iter().collect(),
        }
    }
}

/// The flake's fields as nix prints them, then its inputs as a tree.
fn metadata(v: &Value) -> Vec<Node> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let fields = [
        ("Description", s("description")),
        ("Resolved URL", s("resolvedUrl")),
        ("Locked URL", s("url")),
        ("Path", s("path")),
        ("Revision", s("revision").or_else(|| s("dirtyRevision"))),
        (
            "Revisions",
            v.get("revCount")
                .and_then(Value::as_u64)
                .map(|n| n.to_string()),
        ),
        (
            "Last modified",
            v.get("lastModified").and_then(Value::as_i64).map(date),
        ),
    ];
    let mut kv = ui::kv().key("meta");
    for (k, value) in fields {
        if let Some(value) = value {
            kv = kv.item(k, value);
        }
    }
    let mut nodes: Vec<Node> = vec![kv.into()];
    let locks = v.get("locks");
    let graph = locks.and_then(|l| l.get("nodes"));
    let root = locks
        .and_then(|l| l.get("root"))
        .and_then(Value::as_str)
        .and_then(|r| graph?.get(r));
    if let (Some(graph), Some(root)) = (graph, root) {
        let items = inputs(graph, root, "");
        if !items.is_empty() {
            let mut tree = ui::tree().key("t");
            let count = items.len();
            for item in items {
                tree = tree.item(item);
            }
            nodes.push(
                ui::section()
                    .head(format!("Inputs · {count}"))
                    .collapsible(true)
                    .key("locked")
                    .child(tree)
                    .into(),
            );
        }
    }
    nodes
}

/// The inputs of lock `node`, each with its own inputs folded under it.
fn inputs(graph: &Value, node: &Value, prefix: &str) -> Vec<TreeItem> {
    let Some(own) = node.get("inputs").and_then(Value::as_object) else {
        return Vec::new();
    };
    own.iter()
        .map(|(name, target)| {
            let id = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let mut label = vec![ui::span(name).style("strong")];
            // A `follows` is the path of the input it reuses.
            if let Value::Array(path) = target {
                let path: Vec<&str> = path.iter().filter_map(Value::as_str).collect();
                label.push(ui::span("  follows ").style("dim"));
                label.push(ui::span(path.join("/")).style("muted"));
                return TreeItem::new(id, label);
            }
            let Some(child) = target.as_str().and_then(|k| graph.get(k)) else {
                return TreeItem::new(id, label);
            };
            if let Some(locked) = child.get("locked") {
                label.push(ui::span(format!("  {}", locked_ref(locked))).style("muted"));
                if let Some(t) = locked.get("lastModified").and_then(Value::as_i64) {
                    label.push(ui::span(format!(" · {}", date(t))).style("dim"));
                }
            }
            let mut item = TreeItem::new(&id, label);
            item.children = inputs(graph, child, &id);
            item
        })
        .collect()
}

/// A locked input as a short flake reference.
fn locked_ref(locked: &Value) -> String {
    let s = |k: &str| locked.get(k).and_then(Value::as_str).unwrap_or("");
    let rev = s("rev").get(..7).unwrap_or_else(|| s("rev"));
    match s("type") {
        t @ ("github" | "gitlab" | "sourcehut") => {
            format!("{t}:{}/{}/{rev}", s("owner"), s("repo"))
        }
        "git" | "mercurial" if !rev.is_empty() => format!("{}?rev={rev}", s("url")),
        "git" | "mercurial" | "tarball" | "file" => s("url").to_owned(),
        "path" => s("path").to_owned(),
        "indirect" => s("id").to_owned(),
        t => t.to_owned(),
    }
}

/// The flake's outputs as a tree, like `nix flake show`.
fn show(v: &Value) -> Option<Node> {
    let outputs = v.as_object()?;
    let mut tree = ui::tree().key("t");
    for (name, output) in outputs {
        tree = tree.item(show_item(name, output, name));
    }
    Some(
        ui::section()
            .head("Outputs")
            .key("outputs")
            .child(tree)
            .into(),
    )
}

/// One attribute of the outputs: a leaf has a string `type`, `{}` is one
/// nix did not evaluate (another system), anything else is a set.
fn show_item(name: &str, v: &Value, id: &str) -> TreeItem {
    let empty = Map::new();
    let attrs = v.as_object().unwrap_or(&empty);
    let mut label = vec![ui::span(name).style("strong")];
    if let Some(kind) = attrs.get("type").and_then(Value::as_str) {
        let s = |k: &str| attrs.get(k).and_then(Value::as_str);
        let what = match kind {
            "derivation" => s("name").unwrap_or("derivation"),
            "nixos-configuration" => "NixOS configuration",
            "nixos-module" => "NixOS module",
            "nixpkgs-overlay" => "Nixpkgs overlay",
            other => other,
        };
        label.push(ui::span(format!("  {what}")).style("muted"));
        if let Some(d) = s("description") {
            label.push(ui::span(format!(" · {d}")).style("dim"));
        }
        return TreeItem::new(id, label);
    }
    if attrs.is_empty() {
        // An empty output (no templates) is just empty; deeper down, `{}`
        // is what nix skipped.
        if id != name {
            label.push(ui::span("  omitted").style("dim"));
        }
        return TreeItem::new(id, label);
    }
    // A set where nothing was evaluated (another system) starts folded.
    let evaluated = attrs
        .values()
        .any(|c| c.as_object().is_none_or(|o| !o.is_empty()));
    let mut item = TreeItem::new(id, label);
    item.children = attrs
        .iter()
        .map(|(k, c)| show_item(k, c, &format!("{id}.{k}")))
        .collect();
    item.open(evaluated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_update_lists_each_input() {
        let rev = |c: char| c.to_string().repeat(40);
        let msg = format!(
            "warning: updating lock file '/tmp/f/flake.lock':\n\
             • Updated input 'nixpkgs':\n    'github:NixOS/nixpkgs/{}' (2026-10-01)\n  → 'github:NixOS/nixpkgs/{}' (2026-10-20)\n\
             • Added input 'a/b':\n    follows 'c'\n\
             • Added input 'local':\n    'path:/nix/store/x-source?lastModified=0&narHash=sha256-x%3D' (1970-01-01)\n\
             • Updated input 'tool':\n    'git+https://example.org/tool?ref=main&rev={}' (2026-09-01)\n  → 'git+https://example.org/tool?ref=main&rev={}' (2026-09-02)\n\
             • Removed input 'old'",
            rev('a'),
            rev('b'),
            rev('c'),
            rev('d')
        );
        let pinned = |source: &str, rev: Option<&str>, date: Option<&str>| {
            Some(Lock::Pinned {
                source: source.into(),
                rev: rev.map(Into::into),
                date: date.map(Into::into),
            })
        };
        let changes = lock_changes(&msg).expect("a lock file message");
        assert_eq!(
            changes,
            [
                LockChange {
                    verb: "Updated".into(),
                    input: "nixpkgs".into(),
                    from: pinned("github:NixOS/nixpkgs", Some("aaaaaaa"), Some("2026-10-01")),
                    to: pinned("github:NixOS/nixpkgs", Some("bbbbbbb"), Some("2026-10-20")),
                },
                LockChange {
                    verb: "Added".into(),
                    input: "a/b".into(),
                    from: None,
                    to: Some(Lock::Follows("c".into())),
                },
                LockChange {
                    verb: "Added".into(),
                    input: "local".into(),
                    from: None,
                    to: pinned("path:/nix/store/x-source", None, None),
                },
                LockChange {
                    verb: "Updated".into(),
                    input: "tool".into(),
                    from: pinned(
                        "git+https://example.org/tool",
                        Some("ccccccc"),
                        Some("2026-09-01")
                    ),
                    to: pinned(
                        "git+https://example.org/tool",
                        Some("ddddddd"),
                        Some("2026-09-02")
                    ),
                },
                LockChange {
                    verb: "Removed".into(),
                    input: "old".into(),
                    from: None,
                    to: None,
                },
            ]
        );
        assert!(lock_changes("warning: Git tree is dirty").is_none());
    }

    #[test]
    fn dates_are_utc_calendar_dates() {
        assert_eq!(date(0), "1970-01-01 00:00 UTC");
        // 2024 is a leap year.
        assert_eq!(date(1_709_210_096), "2024-02-29 12:34 UTC");
    }
}
