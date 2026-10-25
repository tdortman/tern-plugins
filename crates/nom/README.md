# tern-nom

[nix-output-monitor](https://github.com/maralorn/nix-output-monitor) (`nom`) for the [Tern](https://stencil.so/tern) terminal.
In a Tern pane, `nom build` and anything that pipes into `nom --json`, such as `nh os switch`, show a native card instead of nom's text table.
The card shows the dependency tree, running builds with estimated progress, downloads, errors, and the build log.
Everywhere else it runs the real `nom`, so nothing changes outside Tern.

## What the card shows

- Built and fetched counts, and a timer.
- What nix does besides builds and downloads, such as evaluating or querying caches, as a log under the counts.
  While nix runs, the log shows its last four lines. Once the run ends, it folds into one line that opens to its last 12 lines and a "Full log" link to all of it.
- At the bottom, a progress bar for builds and a download bar over all paths fetched from caches.
- One row per path being fetched, with its own download bar.
  A fetch gets its row once it has run for a second, so the many that finish sooner do not flash in and out.
- A failed build as its name, exit code, its own last log lines, and the `nix log` command.
  Other errors show as nix prints them, with store paths cut down to the package name.
- The derivations nix builds, as a tree from what you asked for down to its inputs, in a fixed order, with every derivation on its own row.
  Each one shows whether it is waiting, building, built, or failed, with its time.
  A running build's row also has its stdenv phase, its last output line, and how long it has run.
  If nom has timed the same package before, the row also has a progress bar against that estimate.
- Warnings in a folded section, one line each.
  A warning nix prints several times shows once with a count, store paths are cut down to the package name, and quoted option names show as code.
- While nix fetches flake inputs, the current input with its download and the last three done, so the card keeps its height however many inputs there are.
  When nix stops reporting progress after a download because it is unpacking the source, the row says so instead of showing a full bar.
  Once the run ends, the inputs fold into one line with their count, which opens to the full list. Local `path:` and `git+file:` sources are not listed.
- After `nom flake update` or `nom flake lock`, a table of the inputs that were updated, added, or removed: each input's source, and its revision and date before and after.
  A nested input's parent path and a `follows` are dimmed, so the input's own name stands out.
- The paths fetched from caches, in a folded section.
- Build logs stay hidden. Click a running or finished build's row to show the last 8 lines of its log under the row, and click again to hide it.
  The button above those lines expands them to the last 30 lines.
  A failed build's log is always shown in its failure block.
- Output nix prints outside any build, in a folded section.
- Every build's whole log and nix's activity log, without colors, in `$XDG_CACHE_HOME/tern-nom/logs`, one directory per run, keeping the last 10 runs.
  Click "Full log" under an open log, a failure or the activity log to open the file in a Tern file block, where Tern's search finds any line.
  The block reloads as the build prints.
  After `nom` exits, a plain click no longer opens the log. Instead, ⌘-click the link or a derivation's name in the tree, or right-click it and choose Open file.

Ctrl+C stops nix as usual. The card then reads Cancelled and `nom` exits with 130.

## Build-time estimates

tern-nom reads and writes nom's own history, `$XDG_STATE_HOME/nix-output-monitor/build-reports.csv`.
It uses the same columns, lock file, and limit of 10 runs per package as nom.
The estimate for a build is the median of its recorded runs, as in nom.
Your existing nom history gives estimates from the first run, and builds timed by either program improve both.

## Install

The flake's `tern-nom` package installs `nom`, plus `nom-build` and `nom-shell` from nix-output-monitor.
Add the [tern-plugins](../../README.md) flake as an input:

```nix
inputs.tern-plugins = {
	url = "github:tdortman/tern-plugins";
	inputs.nixpkgs.follows = "nixpkgs";
};
```

Put `inputs.tern-plugins.packages.${system}.tern-nom` in your packages instead of `nix-output-monitor`.

nh puts its own copy of nom on its `PATH`. To make nh use tern-nom, use the flake's `nh` package, for example as `programs.nh.package`:

```nix
programs.nh.package = inputs.tern-plugins.packages.${system}.nh;
```

It is nixpkgs' nh with tern-nom as its nom.
In a Tern pane it also adds `--diff never` to `nh os|home|darwin <command>`, because the card shows the diff.
Outside Tern, under a multiplexer, or when you pass `--diff` yourself, nh prints its own diff as usual.

Under `nh os`, `nh home` and `nh darwin`, the card shows what the build changes against the running system or home-manager profile once the build ends.
tern-nom runs [dix](https://github.com/faukah/dix), the tool nh uses for its own diff, with `--output json`.
It finds the new closure through the `--out-link` of the `nix build` that nh runs beside it.
The table lists the system's own packages and every added or removed package, each with its version before and after, the part that changed highlighted, and its size change.
Dependencies whose version or size changed are listed in a second table below it, open by default and collapsible.

## Which commands draw a card

| Command | In a Tern pane |
| --- | --- |
| `nom build`, `nom copy`, `nom flake` | Card. tern-nom runs `nix` with `-v --log-format internal-json`. |
| `nom flake metadata`, `nom flake show` | Card with the flake's fields and inputs, or its outputs, as trees. tern-nom adds `--json` and draws the result. Piped into another program, or given `--json`, nix's own output goes to stdout as usual. |
| `nom develop`, `nom shell` | Card while nix builds the environment, then `nix develop` or `nix shell` with the same arguments, as nom does. |
| `nom --json` | Card. tern-nom reads nix's log from stdin. nh runs nom this way, and so does a direnv `use flake … 2> >(nom --json >&2)`. |
| plain `nom`, `nom-build`, `nom-shell` | The real `nom`. |

Piped into `nom --json`, the card shows once nix builds, fetches, or has run for a second. A direnv reload that finds everything cached leaves no card behind. Under direnv, the card is titled with the project directory.

The card draws only if all of these are true:

- `TERN_PANE` or `TERM_PROGRAM=tern` is set.
- `TERN_TSP` is not `0`.
- tmux, screen, and zellij are not running.
- Tern answers the surface-protocol handshake on `/dev/tty`.

Otherwise tern-nom runs the binary named by `TERN_NOM_REAL` with the same arguments. The package sets `TERN_NOM_REAL` to nix-output-monitor's `nom`.

ssh does not forward `TERN_PANE`, so builds on a remote host get the plain `nom` table.

## Develop

From the repository root:

```sh
nix develop
cargo nextest run -p tern-nom
cargo clippy-strict
nix fmt
```

`crates/nom/tests/` holds three captured Lix logs: a build that succeeds, one that fails, and one that downloads from a cache.
The tests replay them through the parser and check the counts, the dependency tree, which build failed, and the download totals.

## License

MIT
