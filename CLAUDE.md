# Anvil: agent brief

Anvil is a compiled language designed to be written by AI agents. The compiler is in Rust.

## Read first (and only these)
- `docs/SPEC.md`: the whole language (or run `anvil spec`)
- `docs/ROADMAP.md`: milestones; the first unchecked one is current work
- `docs/DECISIONS.md`: why things are the way they are

## Toolchain
- Rust stable, MSVC. If `cargo` is not found, prepend `%USERPROFILE%\.cargo\bin` to PATH.
- Linux works the same way (`cc` links). After a change to the backend, linker or runtime, run
  the suite on Linux too (a WSL distribution with Rust will do).
- `cargo test --workspace` must pass before any milestone is called done.
- `cargo run -p anvil_cli -- <command> <file.anv>` drives the compiler; `anvil-mcp` serves the
  same commands as MCP tools.
- Every `.anv` file must be in canonical layout: run `anvil fmt` (or `anvil fix`) after editing.

## Rules
1. **Both backends agree.** Every language change adds programs to `tests/programs` and must
   give identical stdout, panic text and exit code in the interpreter and native code. Native
   test runs set `ANVIL_CHECK_LEAKS=1`, so a reference-counting mistake fails the test. After a
   change to either backend, run `anvil fuzz --count 50`; a failing seed is a bug to fix.
2. **Every error code is tested.** A new code needs an explanation in `diag::explain`, an entry
   in `ALL_CODES`, and a file in `tests/errors`. Mark it `// resolve: mechanical` if its
   mechanical fixes must produce a clean program.
3. **Fix kinds are honest.** `mechanical` only when the fix cannot change the author's intent.
   If the fix is a guess, use `changes_meaning`; if a wrong guess would compile, offer no fix.
4. **Loud, never silent.** No warnings, no silent defaults, no implicit conversions.
5. **Deterministic.** Same source, same executable bytes, same output. Randomness is the
   one seeded sequence; tests and goldens read the virtual clock, and keep file writes in
   memory. A golden that needs arguments or stdin gets them from `golden/<file>.in`.
6. **Goldens are reviewed, not just approved.** This repo is an Anvil project (`anvil.toml`);
   `anvil golden approve FILE` stores a program's output in `golden/`. Read the output it prints
   and check it by hand first. `anvil golden check --native` must pass.
7. **One implementation per command.** Commands live in `anvil_ops` and in its `COMMANDS`
   table; the CLI and `anvil-mcp` only parse and print, and `anvil lsp` only turns requests into
   those commands. A new command or option needs a tool parameter too, or the parity test fails.
8. **Lints are fixed, not silenced.** Every function runs in a test or a golden-checked main. Use
   `allow untested "reason"` only where a test cannot reach the function, with a real reason.
9. **Small docs.** Tick ROADMAP checkboxes; add a few lines to DECISIONS per milestone.
10. Commit only when the human asks.
