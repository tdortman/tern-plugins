//! Nix's (and Lix's) `--log-format internal-json` stream.

use serde::Deserialize;
use serde_json::Value;

/// `ActivityType` values.
pub mod act {
    /// One store path copied, substituted or downloaded.
    pub const COPY_PATH: u32 = 100;
    /// A download (a NAR or a narinfo); under a copy, that copy's NAR.
    pub const FILE_TRANSFER: u32 = 101;
    /// The goal of a whole build invocation; carries `SetExpected` totals.
    pub const REALISE: u32 = 102;
    /// Every path copy of an invocation; carries path-count progress.
    pub const COPY_PATHS: u32 = 103;
    /// Every build of an invocation; carries build-count progress.
    pub const BUILDS: u32 = 104;
    /// One derivation build.
    pub const BUILD: u32 = 105;
}

/// `ResultType` values.
pub mod res {
    /// A line of a builder's output.
    pub const BUILD_LOG_LINE: u32 = 101;
    /// A stdenv phase started.
    pub const SET_PHASE: u32 = 104;
    /// `[done, expected, running, failed]` of an activity.
    pub const PROGRESS: u32 = 105;
    /// `[activity type, expected]` of a parent activity.
    pub const SET_EXPECTED: u32 = 106;
    /// A line of a post-build hook's output.
    pub const POST_BUILD_LOG_LINE: u32 = 107;
}

/// One `@nix {…}` message.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum Message {
    /// An activity began.
    Start {
        /// Activity id.
        id: u64,
        /// The enclosing activity, 0 for none.
        #[serde(default)]
        parent: u64,
        /// Human-readable description.
        #[serde(default)]
        text: String,
        /// An [`act`] value.
        #[serde(rename = "type", default)]
        kind: u32,
        /// Type-specific fields.
        #[serde(default)]
        fields: Vec<Value>,
    },
    /// An activity ended.
    Stop {
        /// Activity id.
        id: u64,
    },
    /// An update to a running activity.
    Result {
        /// Activity id.
        id: u64,
        /// A [`res`] value.
        #[serde(rename = "type")]
        kind: u32,
        /// Type-specific fields.
        #[serde(default)]
        fields: Vec<Value>,
    },
    /// A log message (errors, warnings, traces, notices).
    Msg {
        /// 0 error, 1 warning, 2 notice, 3 info, higher is chattier.
        #[serde(default)]
        level: u8,
        /// The message, with ANSI colors.
        msg: String,
    },
    /// An action this program does not know.
    #[serde(other)]
    Unknown,
}

/// One line of nix's stderr.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    /// A structured message.
    Nix(Message),
    /// Anything else, as printed.
    Text(String),
}

impl Line {
    /// Parses one line without its line terminator.
    pub fn parse(line: &str) -> Self {
        line.strip_prefix("@nix ")
            .and_then(|json| serde_json::from_str(json).ok())
            .map_or_else(|| Self::Text(line.to_owned()), Self::Nix)
    }
}

/// Field `i` as a string.
pub fn str_field(fields: &[Value], i: usize) -> Option<&str> {
    fields.get(i).and_then(Value::as_str)
}

/// Field `i` as a number.
pub fn num_field(fields: &[Value], i: usize) -> u64 {
    fields.get(i).and_then(Value::as_u64).unwrap_or(0)
}

/// The package name of a store or derivation path:
/// `/nix/store/<hash>-hello-2.12.drv` is `hello-2.12`.
pub fn store_name(path: &str) -> &str {
    let base = path.rsplit('/').next().unwrap_or(path);
    let base = base.strip_suffix(".drv").unwrap_or(base);
    match base.split_once('-') {
        Some((hash, name)) if hash.len() == 32 && !name.is_empty() => name,
        _ => base,
    }
}

/// `text` without ANSI escape sequences.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters, then one final byte in @..~.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or ST.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Every absolute `.drv` path mentioned in `text` (without ANSI escapes).
pub fn drv_paths(text: &str) -> impl Iterator<Item = &str> {
    let path_char = |c: char| c.is_ascii_alphanumeric() || "+-._?=/".contains(c);
    text.split(move |c: char| !path_char(c)).filter(|token| {
        token.starts_with('/')
            && std::path::Path::new(token)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("drv"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_messages_and_passes_other_lines_through() {
        assert_eq!(
            Line::parse(r#"@nix {"action":"stop","id":7}"#),
            Line::Nix(Message::Stop { id: 7 })
        );
        assert_eq!(
            Line::parse(r#"@nix {"action":"msg","level":1,"msg":"warning: dirty"}"#),
            Line::Nix(Message::Msg {
                level: 1,
                msg: "warning: dirty".into()
            })
        );
        assert_eq!(
            Line::parse(r#"@nix {"action":"setPhase","id":7}"#),
            Line::Nix(Message::Unknown)
        );
        assert_eq!(
            Line::parse("@nix {broken"),
            Line::Text("@nix {broken".into())
        );
        assert_eq!(
            Line::parse("plain output"),
            Line::Text("plain output".into())
        );
    }
}
