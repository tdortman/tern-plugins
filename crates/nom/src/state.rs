//! What a nix invocation is doing, folded from its log messages.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::Path,
    time::{Duration, Instant},
};

use crate::{
    diff::Closure,
    drv::{self, DrvInfo},
    flake::{self, LockChange},
    history::{History, report_name},
    logs::Logs,
    nix::{Line, Message, act, drv_paths, num_field, res, store_name, str_field, strip_ansi},
};

/// Reads a `.drv` file's text by path.
pub type DrvReader = Box<dyn Fn(&str) -> Option<String>>;

/// Log lines kept per build.
const LOG_LINES: usize = 400;

/// A derivation being built.
#[derive(Debug)]
pub struct Build {
    /// Its `.drv` path.
    pub drv: String,
    /// Its package name.
    pub name: String,
    /// The remote builder, if not local.
    pub machine: Option<String>,
    /// The current stdenv phase.
    pub phase: Option<String>,
    /// The last non-empty line it printed, without escapes.
    pub last_line: Option<String>,
    /// How long it took before, per nom's history.
    pub estimate: Option<Duration>,
}

/// A store path being copied or substituted.
#[derive(Debug)]
pub struct Copy {
    /// Its store path.
    pub path: String,
    /// Its package name.
    pub name: String,
    /// Where it comes from.
    pub from: Option<String>,
    /// Bytes copied.
    pub done: u64,
    /// Bytes to copy (unpacked NAR size), 0 while unknown.
    pub expected: u64,
}

/// A flake input nix fetches, by the reference it names.
#[derive(Debug)]
pub struct InputFetch {
    /// `github:NixOS/nixpkgs/nixos-unstable`.
    pub reference: String,
    /// Whether nix moved on to the next input or finished.
    pub done: bool,
    /// Bytes its current download got.
    pub bytes: u64,
    /// Bytes its current download expects, 0 while unknown.
    pub expected: u64,
    /// When its last download ended, while nix still works on it (unpacks
    /// and hashes the source) without saying so.
    pub downloaded: Option<Instant>,
}

/// The reference in nix's `fetching github input 'github:a/b'`.
fn fetched_input(plain: &str) -> Option<&str> {
    let rest = plain.trim().strip_prefix("fetching ")?;
    rest.split_once(" input '")?.1.strip_suffix('\'')
}

/// A build or copy that ended.
#[derive(Debug)]
pub struct Finished {
    /// Its `.drv` (builds) or store path (copies).
    pub path: String,
    /// Package name.
    pub name: String,
    /// How long it ran.
    pub took: Duration,
    /// The remote builder, if not local (builds only).
    pub machine: Option<String>,
    /// Whether nix reported it failed.
    pub failed: bool,
    /// Bytes copied (copies only).
    pub bytes: u64,
}

/// How serious a message is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// `error: …`.
    Error,
    /// `warning: …` and level-1 messages.
    Warning,
    /// Traces and notices.
    Info,
}

/// A message nix printed for the user.
#[derive(Debug)]
pub struct Note {
    /// How serious it is.
    pub severity: Severity,
    /// The message, with ANSI colors.
    pub text: String,
    /// How often nix printed it.
    pub count: usize,
}

/// `[done, expected, running, failed]` of a counting activity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Finished successfully.
    pub done: u64,
    /// In total.
    pub expected: u64,
    /// Running now.
    pub running: u64,
    /// Failed.
    pub failed: u64,
}

impl Counts {
    /// From a `Progress` result's fields.
    fn from_fields(fields: &[serde_json::Value]) -> Self {
        Self {
            done: num_field(fields, 0),
            expected: num_field(fields, 1),
            running: num_field(fields, 2),
            failed: num_field(fields, 3),
        }
    }

    /// Field-wise sum.
    const fn add(self, other: Self) -> Self {
        Self {
            done: self.done + other.done,
            expected: self.expected + other.expected,
            running: self.running + other.running,
            failed: self.failed + other.failed,
        }
    }
}

/// How the invocation ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Everything succeeded.
    Done,
    /// Something failed.
    Failed,
    /// The user interrupted it.
    Cancelled,
}

/// Which of nix's plan listings message lines belong to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Listing {
    None,
    Builds,
    Fetches,
}

/// Where a planned derivation is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrvStatus {
    /// Not started.
    Waiting,
    /// Building as this activity.
    Building(u64),
    /// Built in this time.
    Built(Duration),
    /// Its builder failed.
    Failed,
}

/// A planned derivation and the planned derivations it needs.
#[derive(Debug, PartialEq, Eq)]
pub struct DepNode<'a> {
    /// Its `.drv` path.
    pub drv: &'a str,
    /// Its planned inputs (each derivation appears once in the whole tree).
    pub children: Vec<Self>,
}

/// Everything known about the invocation so far.
pub struct State {
    /// The type of every running activity, by id.
    kinds: HashMap<u64, u32>,
    /// When each running build or copy started, by id.
    starts: HashMap<u64, Instant>,
    /// Builds running now, in start order.
    pub builds: BTreeMap<u64, Build>,
    /// Copies running now, in start order.
    pub copies: BTreeMap<u64, Copy>,
    /// Build counts per `Builds` activity.
    build_counts: HashMap<u64, Counts>,
    /// Path counts per `CopyPaths` activity.
    copy_counts: HashMap<u64, Counts>,
    /// Expected copy bytes per `Realise` activity.
    expected_bytes: HashMap<u64, u64>,
    /// Builds that ended, in end order.
    pub built: Vec<Finished>,
    /// Copies that ended, in end order.
    pub copied: Vec<Finished>,
    /// Bytes of the copies that ended.
    copied_bytes: u64,
    /// Derivations nix reported as failed.
    failed_drvs: HashSet<String>,
    /// Errors, warnings and traces.
    pub notes: Vec<Note>,
    /// Inputs a lock file update changed.
    pub lock_changes: Vec<LockChange>,
    /// Flake inputs nix fetched, in order.
    pub inputs: Vec<InputFetch>,
    /// Downloads that belong to the input being fetched.
    input_transfers: HashSet<u64>,
    /// What `nix flake metadata` or `nix flake show` printed.
    pub flake: Option<flake::Output>,
    /// Each build's output by `.drv` path; `""` holds output from outside
    /// builds.
    pub logs: HashMap<String, VecDeque<String>>,
    /// What changes against the current system, under nh.
    pub closure: Option<Closure>,
    /// Every build's whole log on disk, when a directory for it was made.
    pub full_logs: Option<Logs>,
    /// What nix did outside builds, copies and inputs (evaluation, cache
    /// queries), one line each, `\r\n`-ended for Tern's `ansi` element.
    pub activity_log: String,
    /// When monitoring started.
    pub started: Instant,
    /// How long it took and how it ended, once it ended.
    pub outcome: Option<(Outcome, Duration)>,
    /// Derivations nix will build, in the order it named them.
    pub planned: Vec<String>,
    /// What each planned `.drv` file says, `None` when unreadable.
    infos: HashMap<String, Option<DrvInfo>>,
    /// Which of nix's "will be built" / "will be fetched" listings the
    /// current message lines belong to.
    listing: Listing,
    /// Whether nix listed what it will build or fetch.
    saw_plan: bool,
    /// Paths nix said it will fetch.
    planned_fetches: u64,
    /// Reads `.drv` files.
    read_drv: DrvReader,
    /// nom's build-time history.
    pub history: History,
}

impl State {
    /// An invocation that starts now.
    pub fn new(history: History, read_drv: DrvReader) -> Self {
        Self {
            kinds: HashMap::new(),
            starts: HashMap::new(),
            builds: BTreeMap::new(),
            copies: BTreeMap::new(),
            build_counts: HashMap::new(),
            copy_counts: HashMap::new(),
            expected_bytes: HashMap::new(),
            built: Vec::new(),
            copied: Vec::new(),
            copied_bytes: 0,
            failed_drvs: HashSet::new(),
            notes: Vec::new(),
            lock_changes: Vec::new(),
            inputs: Vec::new(),
            input_transfers: HashSet::new(),
            flake: None,
            logs: HashMap::new(),
            closure: None,
            full_logs: None,
            activity_log: String::new(),
            started: Instant::now(),
            outcome: None,
            planned: Vec::new(),
            infos: HashMap::new(),
            listing: Listing::None,
            saw_plan: false,
            planned_fetches: 0,
            read_drv,
            history,
        }
    }

    /// Folds in one line of nix's stderr.
    pub fn apply(&mut self, line: Line) {
        match line {
            Line::Nix(msg) => self.message(msg),
            Line::Text(text) => {
                if !text.trim().is_empty() {
                    self.push_log("", text);
                }
            }
        }
    }

    /// Folds in one structured message.
    fn message(&mut self, msg: Message) {
        match msg {
            Message::Start {
                id,
                text,
                kind,
                fields,
                ..
            } => self.start(id, kind, &text, &fields),
            Message::Stop { id } => self.stop(id),
            Message::Result { id, kind, fields } => self.result(id, kind, &fields),
            Message::Msg { level, msg } => self.note(level, msg),
            Message::Unknown => {}
        }
    }

    /// An activity started.
    fn start(&mut self, id: u64, kind: u32, text: &str, fields: &[serde_json::Value]) {
        self.kinds.insert(id, kind);
        match kind {
            act::BUILD => {
                let drv = str_field(fields, 0).unwrap_or_default().to_owned();
                let machine = str_field(fields, 1)
                    .filter(|m| !m.is_empty())
                    .map(str::to_owned);
                let name = store_name(&drv).to_owned();
                self.plan(&drv);
                let estimate = self.estimate(&drv, machine.as_deref());
                self.starts.insert(id, Instant::now());
                self.builds.insert(
                    id,
                    Build {
                        drv,
                        name,
                        machine,
                        phase: None,
                        last_line: None,
                        estimate,
                    },
                );
            }
            act::COPY_PATH => {
                let path = str_field(fields, 0).unwrap_or_default().to_owned();
                let from = str_field(fields, 1)
                    .filter(|f| !f.is_empty())
                    .map(str::to_owned);
                self.starts.insert(id, Instant::now());
                let name = store_name(&path).to_owned();
                self.copies.insert(
                    id,
                    Copy {
                        path,
                        name,
                        from,
                        done: 0,
                        expected: 0,
                    },
                );
            }
            act::BUILDS => {
                self.build_counts.insert(id, Counts::default());
            }
            act::COPY_PATHS => {
                self.copy_counts.insert(id, Counts::default());
            }
            // Downloads behind a flake input show on its row; NAR and
            // narinfo downloads: their copy or query says it better.
            act::FILE_TRANSFER => {
                if let Some(input) = self.inputs.last_mut().filter(|i| !i.done) {
                    self.input_transfers.insert(id);
                    input.bytes = 0;
                    input.expected = 0;
                    input.downloaded = None;
                }
            }
            _ if !text.is_empty() => self.log_activity(text),
            _ => {}
        }
    }

    /// An activity stopped.
    fn stop(&mut self, id: u64) {
        self.kinds.remove(&id);
        if self.input_transfers.remove(&id)
            && let Some(input) = self.inputs.last_mut().filter(|i| !i.done)
        {
            input.downloaded = Some(Instant::now());
        }
        let took = self
            .starts
            .remove(&id)
            .map_or(Duration::ZERO, |t| t.elapsed());
        if let Some(build) = self.builds.remove(&id) {
            if let Some(logs) = &mut self.full_logs {
                logs.close(&build.drv);
            }
            let failed = self.failed_drvs.contains(&build.drv);
            self.built.push(Finished {
                path: build.drv,
                name: build.name,
                took,
                machine: build.machine,
                failed,
                bytes: 0,
            });
        } else if let Some(copy) = self.copies.remove(&id) {
            let bytes = copy.done.max(copy.expected);
            self.copied_bytes += bytes;
            self.copied.push(Finished {
                path: copy.path,
                name: copy.name,
                took,
                machine: None,
                failed: false,
                bytes,
            });
        }
    }

    /// A result for a running activity.
    fn result(&mut self, id: u64, kind: u32, fields: &[serde_json::Value]) {
        match kind {
            res::BUILD_LOG_LINE | res::POST_BUILD_LOG_LINE => {
                let Some(line) = str_field(fields, 0) else {
                    return;
                };
                let drv = self.builds.get_mut(&id).map(|build| {
                    let plain = strip_ansi(line);
                    let plain = plain.trim();
                    if !plain.is_empty() {
                        build.last_line = Some(plain.to_owned());
                    }
                    build.drv.clone()
                });
                self.push_log(drv.as_deref().unwrap_or_default(), line.to_owned());
            }
            res::SET_PHASE => {
                if let (Some(build), Some(phase)) = (self.builds.get_mut(&id), str_field(fields, 0))
                {
                    build.phase = Some(phase.to_owned());
                }
            }
            res::PROGRESS => match self.kinds.get(&id).copied() {
                Some(act::BUILDS) => {
                    self.build_counts.insert(id, Counts::from_fields(fields));
                }
                Some(act::COPY_PATHS) => {
                    self.copy_counts.insert(id, Counts::from_fields(fields));
                }
                Some(act::COPY_PATH) => {
                    if let Some(copy) = self.copies.get_mut(&id) {
                        copy.done = num_field(fields, 0);
                        copy.expected = num_field(fields, 1);
                    }
                }
                Some(act::FILE_TRANSFER) if self.input_transfers.contains(&id) => {
                    if let Some(input) = self.inputs.last_mut() {
                        input.bytes = num_field(fields, 0);
                        input.expected = num_field(fields, 1);
                    }
                }
                _ => {}
            },
            res::SET_EXPECTED
                if self.kinds.get(&id) == Some(&act::REALISE)
                    && num_field(fields, 0) == u64::from(act::COPY_PATH) =>
            {
                self.expected_bytes.insert(id, num_field(fields, 1));
            }
            _ => {}
        }
    }

    /// A log message.
    fn note(&mut self, level: u8, text: String) {
        let plain = strip_ansi(&text);
        for line in plain.lines() {
            let line = line.trim();
            if line.contains(" will be built") {
                self.listing = Listing::Builds;
                self.saw_plan = true;
            } else if line.contains(" will be fetched") {
                self.listing = Listing::Fetches;
                self.saw_plan = true;
            } else if line.starts_with('/')
                && self.listing == Listing::Builds
                && Path::new(line).extension().is_some_and(|e| e == "drv")
            {
                self.plan(line);
            } else if line.starts_with('/') && self.listing == Listing::Fetches {
                self.planned_fetches += 1;
            } else {
                self.listing = Listing::None;
            }
        }
        // Levels above notice are nix's own progress chatter under `-v`,
        // except the flake inputs it fetches one after another.
        if let Some(reference) = fetched_input(&plain) {
            self.fetch_input(reference);
            return;
        }
        if level > 2 {
            return;
        }
        let head = plain.trim_start();
        // nix reports a lock file update as a warning, once every input is
        // fetched.
        if let Some(changes) = flake::lock_changes(head) {
            if let Some(input) = self.inputs.last_mut() {
                input.done = true;
            }
            self.lock_changes.extend(changes);
            return;
        }
        let severity = if level == 0 && head.starts_with("error") {
            Severity::Error
        } else if level == 1
            || head.starts_with("warning")
            || head.starts_with("evaluation warning")
        {
            Severity::Warning
        } else {
            Severity::Info
        };
        if severity == Severity::Error {
            self.mark_failed(&plain);
        }
        match self.notes.iter_mut().find(|n| n.text == text) {
            Some(seen) => seen.count += 1,
            None => self.notes.push(Note {
                severity,
                text,
                count: 1,
            }),
        }
    }

    /// nix fetches flake inputs one at a time: a new one means the last is
    /// done. Local `path:` and `git+file:` sources take no time to show.
    fn fetch_input(&mut self, reference: &str) {
        if reference.starts_with("path:") || reference.starts_with("git+file:") {
            return;
        }
        if let Some(input) = self.inputs.last_mut() {
            input.done = true;
        }
        self.inputs.push(InputFetch {
            reference: reference.to_owned(),
            done: false,
            bytes: 0,
            expected: 0,
            downloaded: None,
        });
    }

    /// Marks builds an error names as failed; Lix reports a builder's failure
    /// after its activity stopped, nix while it still runs.
    fn mark_failed(&mut self, error: &str) {
        for drv in drv_paths(error) {
            if self.builds.values().any(|b| b.drv == drv) {
                self.failed_drvs.insert(drv.to_owned());
            } else if let Some(done) = self.built.iter_mut().find(|f| f.path == drv) {
                done.failed = true;
            }
        }
    }

    /// Appends to a log, dropping its oldest line when full; a build's line
    /// also goes to its log file.
    fn push_log(&mut self, key: &str, line: String) {
        if let Some(logs) = self.full_logs.as_mut().filter(|_| !key.is_empty()) {
            logs.write(key, &line);
        }
        let log = self.logs.entry(key.to_owned()).or_default();
        if log.len() == LOG_LINES {
            log.pop_front();
        }
        log.push_back(line);
    }

    /// Build counts: from the plan and what was seen when nix listed one,
    /// else nix's own counts (which can stop updating before the last build
    /// ends), else what was seen.
    pub fn build_counts(&self) -> Counts {
        let failed = self.built.iter().filter(|b| b.failed).count() as u64;
        let done = self.built.len() as u64 - failed;
        let running = self.builds.len() as u64;
        if self.saw_plan {
            let expected = (self.planned.len() as u64).max(done + failed + running);
            return Counts {
                done,
                expected,
                running,
                failed,
            };
        }
        if self.build_counts.is_empty() {
            return Counts {
                done,
                expected: done + failed + running,
                running,
                failed,
            };
        }
        self.build_counts
            .values()
            .fold(Counts::default(), |a, &b| a.add(b))
    }

    /// Path-copy counts: from nix's listing and what was seen when nix
    /// listed what it fetches (its own expected count drifts while it
    /// resolves goals), else nix's own, else what was seen.
    pub fn copy_counts(&self) -> Counts {
        let done = self.copied.len() as u64;
        let running = self.copies.len() as u64;
        if self.saw_plan || self.copy_counts.is_empty() {
            let expected = self.planned_fetches.max(done + running);
            return Counts {
                done,
                expected,
                running,
                failed: 0,
            };
        }
        self.copy_counts
            .values()
            .fold(Counts::default(), |a, &b| a.add(b))
    }

    /// Bytes copied and bytes to copy in total.
    pub fn copy_bytes(&self) -> (u64, u64) {
        let running: u64 = self.copies.values().map(|c| c.done).sum();
        let left: u64 = self
            .copies
            .values()
            .map(|c| c.expected.saturating_sub(c.done))
            .sum();
        let done = self.copied_bytes + running;
        let announced: u64 = self.expected_bytes.values().sum();
        (done, announced.max(done + left))
    }

    /// Builds that failed.
    pub fn failed_builds(&self) -> usize {
        self.built.iter().filter(|b| b.failed).count()
    }

    /// Error messages.
    pub fn errors(&self) -> usize {
        self.notes
            .iter()
            .filter(|n| n.severity == Severity::Error)
            .count()
    }

    /// How long ago it started.
    pub fn started_ago(&self, id: u64) -> Duration {
        self.starts
            .get(&id)
            .map_or(Duration::ZERO, Instant::elapsed)
    }

    /// Adds a line to the activity log and its file; tern-nom's own steps
    /// go there too.
    pub fn log_activity(&mut self, text: &str) {
        if let Some(logs) = &mut self.full_logs {
            logs.write(crate::logs::ACTIVITY, text);
        }
        self.activity_log.push_str(text);
        self.activity_log.push_str("\r\n");
    }

    /// Whether a finished run that never showed a card has anything to say:
    /// builds, downloads, warnings, errors, a lock file change or a diff.
    pub fn worth_showing(&self) -> bool {
        !matches!(self.outcome, Some((Outcome::Done, _)))
            || !self.built.is_empty()
            || !self.copied.is_empty()
            || self.notes.iter().any(|n| n.severity != Severity::Info)
            || !self.lock_changes.is_empty()
            || self.closure.is_some()
    }

    /// Ends the invocation; `success` is what the caller knows (nix's exit
    /// status), combined with failures seen in the log.
    pub fn finish(&mut self, outcome: Outcome) {
        let outcome = match outcome {
            Outcome::Done if self.failed_builds() + self.errors() > 0 => Outcome::Failed,
            other => other,
        };
        self.outcome = Some((outcome, self.started.elapsed()));
        // Failures arrive after a build stops, so only now is "built" final.
        let runs: Vec<_> = self
            .built
            .iter()
            .filter(|b| !b.failed)
            .map(|b| {
                (
                    b.machine.clone().unwrap_or_default(),
                    self.report_name(&b.path),
                    b.took.as_secs(),
                )
            })
            .collect();
        for (host, name, secs) in runs {
            self.history.record(&host, &name, secs);
        }
    }

    /// Adds `drv` to the plan and reads its file once.
    fn plan(&mut self, drv: &str) {
        if self.infos.contains_key(drv) {
            return;
        }
        let info = (self.read_drv)(drv).and_then(|text| drv::parse(&text));
        self.infos.insert(drv.to_owned(), info);
        self.planned.push(drv.to_owned());
    }

    /// nom's history key for `drv`.
    fn report_name(&self, drv: &str) -> String {
        let pname = self
            .infos
            .get(drv)
            .and_then(Option::as_ref)
            .and_then(|i| i.pname.as_deref());
        report_name(pname, store_name(drv))
    }

    /// How long `drv` took before on `machine` (`None`: this machine).
    pub fn estimate(&self, drv: &str, machine: Option<&str>) -> Option<Duration> {
        self.history
            .estimate(machine.unwrap_or_default(), &self.report_name(drv))
            // nom records trivial builds as 0 s, which estimates nothing.
            .filter(|&secs| secs > 0)
            .map(Duration::from_secs)
    }

    /// Where `drv` is.
    pub fn status(&self, drv: &str) -> DrvStatus {
        if let Some((&id, _)) = self.builds.iter().find(|(_, b)| b.drv == drv) {
            return DrvStatus::Building(id);
        }
        match self.built.iter().rev().find(|b| b.path == drv) {
            Some(b) if b.failed => DrvStatus::Failed,
            Some(b) => DrvStatus::Built(b.took),
            None => DrvStatus::Waiting,
        }
    }

    /// The planned derivations as a tree from what was asked for down to its
    /// planned inputs; a derivation several others need shows under the first.
    pub fn dependency_tree(&self) -> Vec<DepNode<'_>> {
        let inputs = |drv: &str| -> Vec<&str> {
            let mut out: Vec<&str> = self
                .infos
                .get(drv)
                .and_then(Option::as_ref)
                .map(|i| {
                    i.inputs
                        .iter()
                        .map(String::as_str)
                        .filter(|d| self.infos.contains_key(*d))
                        .collect()
                })
                .unwrap_or_default();
            out.sort_by_key(|d| store_name(d));
            out
        };
        let needed: HashSet<&str> = self.planned.iter().flat_map(|d| inputs(d)).collect();
        let mut roots: Vec<&str> = self
            .planned
            .iter()
            .map(String::as_str)
            .filter(|d| !needed.contains(d))
            .collect();
        roots.sort_by_key(|d| store_name(d));

        fn walk<'a>(
            drv: &'a str,
            inputs: &dyn Fn(&str) -> Vec<&'a str>,
            seen: &mut HashSet<&'a str>,
        ) -> DepNode<'a> {
            let children = inputs(drv)
                .into_iter()
                .filter(|d| seen.insert(d))
                .collect::<Vec<_>>()
                .into_iter()
                .map(|d| walk(d, inputs, seen))
                .collect();
            DepNode { drv, children }
        }
        let mut seen: HashSet<&str> = roots.iter().copied().collect();
        roots
            .into_iter()
            .map(|r| walk(r, &inputs, &mut seen))
            .collect()
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// `.drv` files for the fixtures' graph: `top` needs `c`, `c` needs `a`
    /// and `b`, and each sets `pname` to its first name part.
    fn reader(log: &str) -> DrvReader {
        let mut drvs: Vec<String> = log
            .lines()
            .flat_map(|l| drv_paths(l).map(str::to_owned).collect::<Vec<_>>())
            .collect();
        drvs.sort_unstable();
        drvs.dedup();
        Box::new(move |path| {
            let pname = store_name(path).split('-').next()?;
            let wanted: &[&str] = match pname {
                "top" => &["c"],
                "c" => &["a", "b"],
                _ => &[],
            };
            let inputs: Vec<String> = drvs
                .iter()
                .filter(|d| {
                    wanted
                        .iter()
                        .any(|w| store_name(d).starts_with(&format!("{w}-")))
                })
                .map(|d| format!(r#"("{d}",["out"])"#))
                .collect();
            Some(format!(
                r#"Derive([],[{}],[],"x86_64-linux","/bin/sh",[],[("pname","{pname}")])"#,
                inputs.join(",")
            ))
        })
    }

    /// Feeds a captured log.
    pub fn feed(log: &str) -> State {
        let mut state = State::new(History::default(), reader(log));
        for line in log.lines() {
            state.apply(Line::parse(line));
        }
        state
    }

    /// The tree as `name[children]` strings.
    fn shape(node: &DepNode<'_>) -> String {
        let kids: Vec<String> = node.children.iter().map(shape).collect();
        format!(
            "{}[{}]",
            store_name(node.drv).split('-').next().unwrap_or_default(),
            kids.join(",")
        )
    }

    /// `nix-build -v --log-format internal-json` (Lix 2.96) of four
    /// derivations and one substitution.
    pub const SUCCESS: &str = include_str!("../tests/lix-success.log");

    /// The same where one builder exits 3, failing its dependent.
    const FAILURE: &str = include_str!("../tests/lix-failure.log");

    /// `nix build nixpkgs#gimp` (Lix 2.96): 21 paths substituted, nothing
    /// built, progress thinned.
    const DOWNLOAD: &str = include_str!("../tests/lix-download.log");

    #[test]
    fn downloads_count_from_the_listing_and_track_unpacked_bytes() {
        let mut state = State::new(History::default(), reader(DOWNLOAD));
        for line in DOWNLOAD.lines() {
            state.apply(Line::parse(line));
            // Lix's own count claims 65 paths at this point.
            if line.contains(r#""fields":[19,65,1,0]"#) {
                assert_eq!(state.copy_counts().expected, 21);
                // The bar's total is the announced unpacked size, not a sum
                // that grows with each finished path.
                let (done, total) = state.copy_bytes();
                assert!(done < total && total == 690_460_208, "{done} / {total}");
            }
            assert!(!state.activity_log.contains("downloading"));
        }
        assert_eq!(state.build_counts().expected, 0);
        assert_eq!(
            state.copy_counts(),
            Counts {
                done: 21,
                expected: 21,
                running: 0,
                failed: 0
            }
        );
        let (done, total) = state.copy_bytes();
        assert!(done <= total && total == 690_460_208, "{done} / {total}");
    }

    #[test]
    fn plan_becomes_a_dependency_tree_with_statuses() {
        let mut state = feed(FAILURE);
        // Planned from nix's listing before any build started.
        assert_eq!(state.planned.len(), 4);
        let tree = state.dependency_tree();
        assert_eq!(
            tree.iter().map(shape).collect::<Vec<_>>(),
            ["top[c[a[],b[]]]"]
        );
        let c = tree[0].children[0].drv;
        let a = tree[0].children[0].children[0].drv;
        assert!(matches!(state.status(a), DrvStatus::Built(_)));
        assert_eq!(state.status(c), DrvStatus::Failed);
        assert_eq!(state.status(tree[0].drv), DrvStatus::Waiting);
        state.finish(Outcome::Done);
        assert_eq!(state.outcome.map(|o| o.0), Some(Outcome::Failed));
    }

    #[test]
    fn successful_build_counts_builds_copies_and_bytes() {
        let mut state = feed(SUCCESS);
        state.finish(Outcome::Done);
        assert_eq!(
            state.build_counts(),
            Counts {
                done: 4,
                expected: 4,
                running: 0,
                failed: 0
            }
        );
        assert_eq!(state.copy_counts().done, 1);
        assert_eq!(state.copy_bytes(), (279_592, 279_592));
        assert!(state.builds.is_empty() && state.copies.is_empty());
        let mut names: Vec<_> = state.built.iter().map(|b| b.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["a-18439", "b-18439", "c-18439", "top-18439"]);
        assert_eq!(state.copied[0].name, "hello-2.12.3");
        // Each build's log is its own, without a name prefix.
        let top = state
            .planned
            .iter()
            .find(|d| d.contains("-top-"))
            .expect("top is planned");
        assert_eq!(
            state.logs[top].back().map(String::as_str),
            Some("top step 3")
        );
        assert_eq!(state.errors(), 0);
        assert_eq!(state.outcome.map(|o| o.0), Some(Outcome::Done));
    }

    #[test]
    fn failed_builder_is_marked_and_fails_the_outcome() {
        let mut state = feed(FAILURE);
        state.finish(Outcome::Done);
        let failed: Vec<_> = state
            .built
            .iter()
            .filter(|b| b.failed)
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(failed, ["c-5863"]);
        assert_eq!(state.errors(), 2);
        assert_eq!(state.outcome.map(|o| o.0), Some(Outcome::Failed));
    }
}
