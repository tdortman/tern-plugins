//! `nom`, drawn natively when it runs in a Tern pane.
//!
//! `nom build|copy|flake …` and `nom --json` (how `nh` pipes nix into it)
//! become a live Tern card; everything else, and every run outside Tern,
//! is handed to the real nix-output-monitor named by `TERN_NOM_REAL`.

mod diff;
mod drv;
mod flake;
mod history;
mod logs;
mod nix;
mod state;
mod tty;
mod view;

use std::{
    io::{self, BufRead as _, BufReader, Read, Write as _},
    os::unix::process::{CommandExt as _, ExitStatusExt as _},
    path::Path,
    process::{Command, ExitCode, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use state::{Outcome, State};
use tern_sdk::{Input, Options, Session, Surface, SurfaceOptions, ui::View};
use view::Msg;

/// How often the card is redrawn while output arrives.
const FRAME: Duration = Duration::from_millis(100);

/// How long one wait for terminal input lasts.
const POLL: Duration = Duration::from_millis(30);

/// How often running builds' timers and estimates are redrawn.
const TICK: Duration = Duration::from_secs(1);

/// How long piped input may run before its card shows without a build.
const QUIET: Duration = Duration::from_secs(1);

/// Set by SIGINT/SIGTERM.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

/// Where nix's log comes from.
enum Source {
    /// `nix <args>` run by this process.
    Spawn(Vec<String>),
    /// A pipe on stdin (`nom --json`).
    Stdin,
}

/// Replaces this process with the real nom.
fn real_nom(args: &[String]) -> ExitCode {
    let Some(real) = std::env::var_os("TERN_NOM_REAL") else {
        eprintln!("tern-nom: TERN_NOM_REAL is not set; it names the real nix-output-monitor's nom");
        return ExitCode::FAILURE;
    };
    let err = Command::new(real).arg0("nom").args(args).exec();
    eprintln!("tern-nom: cannot run the real nom: {err}");
    ExitCode::FAILURE
}

/// Whether this runs in a Tern pane a card can draw in: Tern's variables
/// set and no multiplexer, which would swallow the handshake.
fn in_tern_pane() -> bool {
    let set = |name| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    // shortcut: no TSP over ssh (TERN_PANE isn't forwarded), revisit if builds run remotely.
    std::env::var_os("TERN_TSP").is_none_or(|v| v != "0")
        && !set("TMUX")
        && !set("STY")
        && !set("ZELLIJ")
        && (set("TERN_PANE") || std::env::var_os("TERM_PROGRAM").is_some_and(|v| v == "tern"))
}

/// A Tern session on the controlling terminal, when there is one.
fn connect() -> Option<Session<Msg>> {
    // Outside a Tern pane the handshake would only cost its timeout.
    if !in_tern_pane() {
        return None;
    }
    let tty = tty::DevTty::open().ok()?;
    let options = Options::new()
        .app("nom")
        .version(env!("CARGO_PKG_VERSION"))
        .bracketed_paste(false)
        .kitty_keyboard(false)
        .timeout(Duration::from_millis(500));
    Session::with_terminal(tty, options).ok().flatten()
}

/// `nom --nh <nh> <args>`, installed as `nh`: runs nh, and in a Tern pane
/// adds `--diff never` to `nh os|home|darwin <verb>`, whose package diff
/// the card draws.
#[allow(unsafe_code, reason = "signal handling, subreaper and reaping for nh")]
fn nh(real: &str, args: &[String]) -> ExitCode {
    if !in_tern_pane() {
        let err = Command::new(real).arg0("nh").args(args).exec();
        eprintln!("tern-nom: cannot run nh: {err}");
        return ExitCode::FAILURE;
    }
    let mut args = args.to_vec();
    let chose = args
        .iter()
        .any(|a| a == "--diff" || a == "-d" || a.starts_with("--diff="));
    if !chose {
        let at = args.windows(2).position(|w| {
            matches!(w[0].as_str(), "os" | "home" | "darwin")
                && matches!(
                    w[1].as_str(),
                    "switch" | "boot" | "test" | "build" | "build-vm"
                )
        });
        if let Some(i) = at {
            args.splice(i + 2..i + 2, ["--diff".to_owned(), "never".to_owned()]);
        }
    }
    // Ctrl+C kills nh at once while its nom still restores the terminal.
    // Were nh this process, the shell would take the terminal back in nom's
    // raw mode and keep it (no Enter for sudo), and nom's restore would
    // fail with EIO. So nh runs as a child, and nom and nix, handed to this
    // process as subreaper when nh dies, are waited for first. A handler,
    // not SIG_IGN, so nh still gets Ctrl+C.
    // SAFETY: the handler only stores to an atomic; PR_SET_CHILD_SUBREAPER
    // takes a flag and has no other preconditions.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1);
    }
    let status = match Command::new(real).arg0("nh").args(&args).status() {
        Ok(s) => s,
        Err(err) => {
            eprintln!("tern-nom: cannot run nh: {err}");
            return ExitCode::FAILURE;
        }
    };
    // SAFETY: getpgrp has no preconditions; waitpid only reaps children in
    // this process group, so a daemonized ssh master is left alone.
    unsafe {
        let job = libc::getpgrp();
        while libc::waitpid(-job, std::ptr::null_mut(), 0) > 0 {}
    }
    if let Some(sig) = status.signal() {
        // Dying of nh's signal tells the shell what Ctrl+C means to it.
        // SAFETY: restoring the default action and raising have no
        // preconditions.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }
    ExitCode::from(
        status
            .code()
            .and_then(|c| u8::try_from(c).ok())
            .unwrap_or(1),
    )
}

/// The command line of this process's parent (`nh os switch`), for the
/// card's title under a pipe; direnv's long stdlib call reads as
/// `direnv ~/project`.
fn parent_command() -> Option<String> {
    let args = diff::cmdline(std::os::unix::process::parent_id())?;
    if let Some(envrc) = args.iter().find_map(|a| a.split_once("source_env ")) {
        let path = envrc.1.trim_start_matches('$').trim_matches('\'');
        let dir = Path::new(path).parent().unwrap_or_else(|| Path::new(path));
        let dir = dir.to_string_lossy();
        let home = std::env::var("HOME").unwrap_or_default();
        return Some(match dir.strip_prefix(&home) {
            Some(rest) if !home.is_empty() => format!("direnv ~{rest}"),
            _ => format!("direnv {dir}"),
        });
    }
    let mut title = diff::program(&args)?.to_owned();
    for arg in &args[1..] {
        title.push(' ');
        title.push_str(arg);
    }
    // What the `nh` wrapper added is not what was typed.
    Some(title.replace(" --diff never", ""))
}

/// Streams `input` line by line into `tx`.
fn pump_lines(input: impl Read + Send + 'static, tx: mpsc::Sender<nix::Line>) {
    thread::spawn(move || {
        let mut input = BufReader::new(input);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match input.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    let line = line.trim_end_matches(['\n', '\r']);
                    if tx.send(nix::Line::parse(line)).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

/// Shows `path` in a Tern file block beside this pane.
fn open_in_tern(path: &str) {
    let spawned = Command::new("tern")
        .args(["open", path])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    // Without `tern` on PATH the link still opens with Ctrl/⌘-click.
    if let Ok(mut child) = spawned {
        thread::spawn(move || child.wait());
    }
}

/// Draws `view`, opening the card's surface on first use.
fn draw(
    session: &mut Session<Msg>,
    sf: &mut Option<Surface>,
    title: &str,
    view: View<Msg>,
) -> Result<(), tern_sdk::Error> {
    let surface = match *sf {
        Some(s) => s,
        None => *sf.insert(session.open(SurfaceOptions::flow().title(title.to_owned()))?),
    };
    session.render(surface, view)
}

/// Monitors `source` on `session`; the process's exit code.
fn monitor(
    mut session: Session<Msg>,
    source: Source,
    title: &str,
) -> Result<ExitCode, tern_sdk::Error> {
    // A pipe (direnv, nh) shows a card once nix does something worth one, so
    // a cached reload stays quiet; a typed command shows one at once.
    let eager = matches!(source, Source::Spawn(_));
    let (tx, rx) = mpsc::channel();
    let mut child = None;
    let mut stdout = None;
    // Under `nh os|home|darwin`, what it compares once the build ends; read
    // now, while nh's `nix build` still runs.
    let mut compare = None;
    let started = SystemTime::now();
    // `nix flake metadata`/`show` for a terminal: the card draws their JSON
    // instead of nix's text, which a pipe still gets.
    let mut drawn: Option<String> = None;
    match source {
        Source::Spawn(args) => {
            let mut cmd = Command::new("nix");
            let (sub, rest) = args.split_first().expect("a subcommand");
            cmd.arg(sub)
                .args(["-v", "--log-format", "internal-json"])
                .args(rest);
            // SAFETY: isatty has no preconditions.
            #[allow(unsafe_code, reason = "isatty")]
            let terminal = unsafe { libc::isatty(libc::STDOUT_FILENO) } == 1;
            if let Some(flake_sub) = rest
                .first()
                .filter(|s| sub == "flake" && flake::Output::drawn(s))
                && terminal
                && !rest.iter().any(|a| a == "--json")
            {
                cmd.arg("--json");
                drawn = Some(flake_sub.clone());
            }
            let mut spawned = match cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
                Ok(c) => c,
                Err(e) => {
                    session.close()?;
                    eprintln!("tern-nom: cannot run nix: {e}");
                    return Ok(ExitCode::FAILURE);
                }
            };
            pump_lines(spawned.stderr.take().expect("piped stderr"), tx);
            let mut out = spawned.stdout.take().expect("piped stdout");
            // Printed after the card closes, as nom does (`--print-out-paths`).
            stdout = Some(thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = out.read_to_end(&mut bytes);
                bytes
            }));
            child = Some(spawned);
        }
        Source::Stdin => {
            compare = diff::target(std::os::unix::process::parent_id());
            pump_lines(io::stdin(), tx);
        }
    }

    let mut sf: Option<Surface> = None;
    let reader: state::DrvReader = Box::new(|path| std::fs::read_to_string(path).ok());
    let mut state = State::new(history::History::load(), reader);
    state.full_logs = logs::Logs::create();
    // The `.drv` whose log the user opened by clicking its row.
    let mut open: Option<String> = None;
    if eager {
        draw(
            &mut session,
            &mut sf,
            title,
            view::view(&state, title, None),
        )?;
    }
    let mut last = Instant::now();
    let mut dirty = false;
    loop {
        if INTERRUPTED.load(Ordering::Relaxed) {
            state.finish(Outcome::Cancelled);
            break;
        }
        // A click on a running build opens or closes its log, one on "Full
        // log" opens the file; reading also lets acks free frames.
        match session.next(Some(POLL))? {
            Some(Input::Msg(Msg::ToggleLog(drv), _)) => {
                open = if open.as_deref() == Some(drv.as_str()) {
                    None
                } else {
                    Some(drv)
                };
                draw(
                    &mut session,
                    &mut sf,
                    title,
                    view::view(&state, title, open.as_deref()),
                )?;
                last = Instant::now();
            }
            Some(Input::Msg(Msg::OpenFile(path), _)) => {
                if let Some(logs) = &mut state.full_logs {
                    logs.flush();
                }
                open_in_tern(&path);
            }
            _ => {}
        }
        let ended = loop {
            // What nix logs while it dies (builds stopping) is not progress.
            if INTERRUPTED.load(Ordering::Relaxed) {
                break false;
            }
            match rx.try_recv() {
                Ok(line) => {
                    state.apply(line);
                    dirty = true;
                }
                Err(mpsc::TryRecvError::Empty) => break false,
                Err(mpsc::TryRecvError::Disconnected) => break true,
            }
        };
        if ended {
            break;
        }
        // Timers, estimate bars and an input's quiet unpacking are text, and
        // rows appear after a second, so redraw while anything runs.
        let busy = !(state.builds.is_empty() && state.copies.is_empty())
            || state.inputs.last().is_some_and(|i| !i.done);
        let tick = busy && last.elapsed() >= TICK;
        let shown = sf.is_some()
            || !state.builds.is_empty()
            || !state.copies.is_empty()
            || state.started.elapsed() >= QUIET;
        if shown && ((dirty && last.elapsed() >= FRAME) || tick) {
            if let Some(logs) = &mut state.full_logs {
                logs.flush();
            }
            draw(
                &mut session,
                &mut sf,
                title,
                view::view(&state, title, open.as_deref()),
            )?;
            last = Instant::now();
            dirty = false;
        }
    }

    // A Ctrl+C reached nix (same process group) too, so this wait is short.
    let status = child.as_mut().map(std::process::Child::wait).transpose()?;
    let mut printed = stdout.and_then(|h| h.join().ok());
    if let (Some(sub), Some(bytes)) = (&drawn, &printed) {
        state.flake = flake::Output::parse(sub, bytes);
        // JSON the card cannot draw still reaches the user.
        if state.flake.is_some() {
            printed = None;
        }
    }
    if state.outcome.is_none() {
        let ok = status.is_none_or(|s| s.success());
        if let Some(target) = compare.as_ref().filter(|_| ok) {
            state.log_activity("comparing with the current system");
            draw(
                &mut session,
                &mut sf,
                title,
                view::view(&state, title, open.as_deref()),
            )?;
            state.closure = target.diff(started);
        }
        state.finish(if ok { Outcome::Done } else { Outcome::Failed });
    }
    if let Some(logs) = &mut state.full_logs {
        logs.flush();
    }
    if sf.is_some() || state.worth_showing() {
        draw(
            &mut session,
            &mut sf,
            title,
            view::view(&state, title, open.as_deref()),
        )?;
    }
    session.close()?;
    if let Err(e) = state.history.save() {
        eprintln!("tern-nom: cannot save build times to nom's history: {e}");
    }

    if let Some(bytes) = printed {
        let mut out = io::stdout().lock();
        let _ = out.write_all(&bytes);
        let _ = out.flush();
    }
    Ok(match (state.outcome.map(|o| o.0), status) {
        (Some(Outcome::Cancelled), _) => ExitCode::from(130),
        (_, Some(s)) => ExitCode::from(s.code().and_then(|c| u8::try_from(c).ok()).unwrap_or(1)),
        (Some(Outcome::Failed), None) => ExitCode::FAILURE,
        _ => ExitCode::SUCCESS,
    })
}

/// `args` (`develop …`/`shell …`) only building the environment, as nom
/// does: anything from `--command` on becomes `--command sh -c exit`.
fn build_only(args: &[String]) -> Vec<String> {
    let mut out: Vec<String> = args
        .iter()
        .take_while(|a| *a != "--command" && *a != "-c")
        .cloned()
        .collect();
    out.extend(["--command", "sh", "-c", "exit"].map(String::from));
    out
}

#[allow(unsafe_code, reason = "installing and resetting signal handlers")]
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let help = args.iter().any(|a| a == "--help");
    // `nom develop`/`nom shell` build in a card, then run nix for real.
    let mut then = None;
    let source = match args.first().map(String::as_str) {
        Some("--nh") if args.len() >= 2 => return nh(&args[1], &args[2..]),
        Some("build" | "copy" | "flake") if !help => Source::Spawn(args.clone()),
        Some("develop" | "shell") if !help => {
            then = Some(args.clone());
            Source::Spawn(build_only(&args))
        }
        Some("--json") if args.len() == 1 => Source::Stdin,
        _ => return real_nom(&args),
    };
    let Some(session) = connect() else {
        return real_nom(&args);
    };
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
    let title = match &source {
        Source::Spawn(_) => format!("nom {}", args.join(" ")),
        Source::Stdin => parent_command().unwrap_or_else(|| "nix".to_owned()),
    };
    let code = match monitor(session, source, &title) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("tern-nom: {e}");
            ExitCode::FAILURE
        }
    };
    match then {
        Some(nix_args) if code == ExitCode::SUCCESS => {
            // SAFETY: restoring the default action has no preconditions.
            unsafe {
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::signal(libc::SIGTERM, libc::SIG_DFL);
            }
            let err = Command::new("nix").args(nix_args).exec();
            eprintln!("tern-nom: cannot run nix: {err}");
            ExitCode::FAILURE
        }
        _ => code,
    }
}
