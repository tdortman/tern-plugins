# tern-plugins

Plugins for the [Tern](https://stencil.so/tern) terminal.
Each one is a Rust crate under `crates/` that draws native cards in a Tern pane over the Tern Surface Protocol, and runs the plain program it replaces everywhere else.

| Plugin | Crate | What it is for |
| --- | --- | --- |
| [tern-nom](crates/nom/README.md) | `crates/nom` | [nix-output-monitor](https://github.com/maralorn/nix-output-monitor) as a live card: builds, downloads, flake updates, and nh's closure diff. |

## Install

Add the flake as an input:

```nix
inputs.tern-plugins = {
	url = "github:tdortman/tern-plugins";
	inputs.nixpkgs.follows = "nixpkgs";
};
```

Each plugin is a package of the same name in `inputs.tern-plugins.packages.${system}`.
The plugin's README says what to replace with it.

## Develop

The crates form one Cargo workspace with one `Cargo.lock`.
Dependencies that more than one crate uses go in `[workspace.dependencies]` in the root `Cargo.toml`, and each crate takes them with `name.workspace = true`.
Lints are set once in `[workspace.lints]`: Clippy's pedantic and nursery groups, every warning an error, and no `unsafe` without an `#[allow(unsafe_code, reason = "…")]` and a `// SAFETY:` comment. Each crate opts in with `[lints] workspace = true`.

```sh
nix develop
cargo nextest run
cargo clippy-strict
nix fmt
```

To add a plugin, create `crates/<name>` with its own `Cargo.toml`, and add its Nix package as `nix/packages/<package>/default.nix`.
Build it with `cargoBuildFlags = [ "-p" "<package>" ]`, as `nix/packages/tern-nom/default.nix` does.

## License

MIT
