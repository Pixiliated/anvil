# Contributing to Anvil

Anvil is built to be written by AI agents, and it is mostly built by them too: the rules here
are the ones CLAUDE.md gives every agent, for people as much as for models. Read
docs/SPEC.md (the language), docs/ROADMAP.md (what comes next: the first unchecked milestone) and
docs/DECISIONS.md (why things are as they are) before changing anything.

## Building and checking

You need Rust (stable) and, for native code, the platform's linker: the Visual Studio C++ build
tools on Windows x64, a C compiler on Linux and macOS.

```bash
cargo test --workspace
```

```bash
cargo run -p anvil-lang -- golden check --native
```

```bash
cargo run -p anvil-lang -- fuzz --count 50
```

A change is done when all three pass, `cargo clippy --workspace --all-targets -- -D warnings`
and `cargo fmt --all -- --check` are clean, and every `.anv` file is in canonical layout
(`anvil fmt --check`). CI runs the same on Windows and Linux.

## The rules

- **Both backends agree.** A language change adds programs to `tests/programs` whose output,
  panic and exit code are the same in the interpreter and in native code, with the leak check
  on (`ANVIL_CHECK_LEAKS=1`).
- **Goldens are read before they are approved.** `anvil golden approve FILE` stores what a
  program printed; check it by hand first.
- **Every error code is tested.** A new code has an explanation (`anvil explain`), an entry in
  `ALL_CODES` and a file in `tests/errors`. A fix is `mechanical` only when it cannot change what
  the author meant.
- **Loud, never silent.** No warnings, no silent defaults, no implicit conversions.
- **Deterministic.** The same source gives the same executable bytes and the same output.
- **One implementation per command.** Commands live in `anvil_ops` (its `COMMANDS` table); the
  CLI, the MCP server and the language server only call them, and a test keeps the CLI and MCP
  in step.
- **Small docs.** Tick the roadmap's boxes; add a few lines to DECISIONS per milestone; record
  each version in CHANGELOG.md.

## Copies

A crate's package holds only its own directory, so a few files are copied into crates:
docs/SPEC.md into `crates/anvil_ops/`, README.md into `crates/anvil_cli/`, and the two license
files into every crate. A test fails when a copy differs; copy the original over it.

## Licence

Contributions are accepted under the project's terms: MIT or Apache-2.0, at the user's option.
