//! nom's build-time history, `$XDG_STATE_HOME/nix-output-monitor/build-reports.csv`.
//!
//! Shared with nom: the same file, columns, time format, lock file and the
//! last 10 runs per (host, name), so estimates carry over in both directions.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs::{self, File},
    io,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

const HEADER: &str = "hostname,derivation name,utc time,build seconds";
/// Runs kept per (host, name), as nom keeps them.
const KEEP: usize = 10;

/// Build seconds by `(host, name)`, keyed by end time (`%F %T`, UTC).
type Runs = BTreeMap<(String, String), BTreeMap<String, u64>>;

/// The history file and what it held when loaded.
#[derive(Debug, Default)]
pub struct History {
    /// The state directory, when one is known.
    dir: Option<PathBuf>,
    runs: Runs,
    /// Builds that finished in this run, not yet saved.
    new: Vec<(String, String, String, u64)>,
}

impl History {
    /// Loads nom's history; empty when there is none.
    pub fn load() -> Self {
        let dir = std::env::var_os("XDG_STATE_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
            .map(|d| d.join("nix-output-monitor"));
        let runs = dir
            .as_ref()
            .and_then(|d| fs::read_to_string(d.join("build-reports.csv")).ok())
            .map(|text| parse(&text))
            .unwrap_or_default();
        Self {
            dir,
            runs,
            new: Vec::new(),
        }
    }

    /// The median of the recorded build times of `name` on `host` (`""` is
    /// this machine), as nom estimates.
    pub fn estimate(&self, host: &str, name: &str) -> Option<u64> {
        let runs = self.runs.get(&(host.to_owned(), name.to_owned()))?;
        let mut secs: Vec<u64> = runs.values().copied().collect();
        secs.sort_unstable();
        let mid = secs.len() / 2;
        match secs.len() {
            0 => None,
            n if n % 2 == 1 => Some(secs[mid]),
            _ => Some(u64::midpoint(secs[mid - 1], secs[mid])),
        }
    }

    /// Notes a build that finished now.
    pub fn record(&mut self, host: &str, name: &str, secs: u64) {
        self.new
            .push((host.to_owned(), name.to_owned(), utc_now(), secs));
    }

    /// Merges this run's builds into the file under nom's lock.
    pub fn save(&self) -> io::Result<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        if self.new.is_empty() {
            return Ok(());
        }
        fs::create_dir_all(dir)?;
        let lock = File::create(dir.join("build-reports.csv.lock"))?;
        // `flock` on Unix, as nom takes it; released when `lock` closes.
        lock.lock()?;
        let path = dir.join("build-reports.csv");
        // Re-read under the lock: nom may have written since `load`.
        let mut runs = fs::read_to_string(&path)
            .map(|t| parse(&t))
            .unwrap_or_default();
        for (host, name, time, secs) in &self.new {
            let entry = runs.entry((host.clone(), name.clone())).or_default();
            entry.insert(time.clone(), *secs);
            while entry.len() > KEEP {
                entry.pop_first();
            }
        }
        fs::write(&path, render(&runs))
    }
}

/// nom's report name: `pname`, else the name without its version suffix.
pub fn report_name(pname: Option<&str>, name: &str) -> String {
    pname.map_or_else(
        || {
            name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '-')
                .to_owned()
        },
        str::to_owned,
    )
}

/// Splits one CSV record (RFC 4180 quoting).
fn fields(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            ('"', _) => quoted = !quoted,
            (',', false) => out.push(std::mem::take(&mut field)),
            (c, _) => field.push(c),
        }
    }
    out.push(field);
    out
}

fn parse(text: &str) -> Runs {
    let mut runs = Runs::new();
    for line in text.lines().skip(1) {
        if let [host, name, time, secs] = fields(line.trim_end_matches('\r')).as_slice()
            && let Ok(secs) = secs.parse()
        {
            runs.entry((host.clone(), name.clone()))
                .or_default()
                .insert(time.clone(), secs);
        }
    }
    runs
}

fn quote(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_owned()
    }
}

fn render(runs: &Runs) -> String {
    let mut out = format!("{HEADER}\r\n");
    for ((host, name), times) in runs {
        for (time, secs) in times {
            let _ = write!(out, "{},{},{time},{secs}\r\n", quote(host), quote(name));
        }
    }
    out
}

/// Now as `YYYY-MM-DD HH:MM:SS` in UTC.
fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Howard Hinnant's days-to-civil.
    let z = days.cast_signed() + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_the_median_and_keeps_nom_format_on_save() {
        let dir = std::env::temp_dir().join(format!("tern-nom-history-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir is writable");
        let csv = dir.join("build-reports.csv");
        fs::write(
            &csv,
            "hostname,derivation name,utc time,build seconds\r\n\
             ,hello,2026-01-01 00:00:00,10\r\n\
             ,hello,2026-01-02 00:00:00,30\r\n\
             ,hello,2026-01-03 00:00:00,20\r\n\
             ssh://box,\"a,b\",2026-01-01 00:00:00,7\r\n",
        )
        .expect("temp file is writable");
        let mut history = History {
            dir: Some(dir.clone()),
            runs: parse(&fs::read_to_string(&csv).expect("just written")),
            new: Vec::new(),
        };
        assert_eq!(history.estimate("", "hello"), Some(20));
        assert_eq!(history.estimate("ssh://box", "a,b"), Some(7));
        assert_eq!(history.estimate("", "missing"), None);

        for secs in 0..10 {
            let time = format!("2026-02-01 00:00:{secs:02}");
            history
                .new
                .push((String::new(), "hello".to_owned(), time, secs));
        }
        history.save().expect("save succeeds");
        let saved = parse(&fs::read_to_string(&csv).expect("saved"));
        // 3 old + 10 new runs: the 3 oldest drop, the quoted name survives.
        let hello = &saved[&(String::new(), "hello".to_owned())];
        assert_eq!(hello.len(), KEEP);
        assert_eq!(
            hello.keys().next().map(String::as_str),
            Some("2026-02-01 00:00:00")
        );
        assert_eq!(saved[&("ssh://box".to_owned(), "a,b".to_owned())].len(), 1);
        fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn report_names_drop_versions_like_nom() {
        assert_eq!(report_name(Some("hello"), "hello-2.12.3"), "hello");
        assert_eq!(
            report_name(None, "linux-7.2.11-modules"),
            "linux-7.2.11-modules"
        );
        assert_eq!(report_name(None, "source-2.0"), "source");
    }
}
