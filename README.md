# Anvil

A small compiled programming language designed to be written mostly by AI agents and read by
people.

```
unit m;
unit s;

fn speed(distance: f64[m], time: f64[s]) -> f64[m/s]
    requires time > 0.0[s]
{
    return distance / time;
}

fn main() uses io {
    print(speed(distance: 100.0[m], time: 9.58[s]));   // 10.438413361169102 m/s
}

test "speed is positive" {
    assert(speed(distance: 1.0[m], time: 1.0[s]) > 0.0[m/s]);
}
```

## Why another language

Agents write code quickly. They are worse at knowing when that code is right. Anvil moves as
much of that judgement as possible into the compiler:

- **One obvious way.** One loop per job, one way to bind, one way to call. Variation between
  runs and between agents shrinks, and so does the room for plausible-but-wrong code.
- **Errors that carry their fix.** Every error has a stable code, an exact range, and fixes
  labelled `mechanical` (safe to apply) or `changes_meaning` (compiles, but may hide the real
  bug). `--json` gives the same thing to tools.
- **Local and explicit.** No operator overloading, macros, implicit conversions, shadowing or
  global state. Every binding states its type. Calls with two or more arguments name them,
  so swapped arguments cannot compile.
- **Checks built in.** Units of measure (`f64[m/s]`), effects (`uses io`), contracts
  (`requires`/`ensures`) and `test` blocks are part of the language, not libraries.
- **Deterministic and loud.** Integer overflow panics. Output is byte-identical on every run
  and platform. The same source gives a byte-identical executable.

The whole language is in [docs/SPEC.md](docs/SPEC.md), which is short enough to give an agent
in full (`anvil spec` prints it).

## Status

v0.1: single-file programs with i64, f64 with units, bool, str, structs, functions, loops,
contracts, effects and tests. There are two backends:

- an **interpreter**, which is the reference semantics;
- a **native compiler**: Cranelift to x86-64, linked with MSVC on Windows.

The test suite runs every program through both and requires identical stdout, panic messages
and exit codes. Arrays, enums, modules and other platforms are on the
[roadmap](docs/ROADMAP.md).

## Using it

You need Rust (stable). Native builds also need Windows x64 with the Visual Studio C++ build
tools.

```bash
cargo build --release
```

```bash
target/release/anvil run examples/projectile.anv
```

```bash
target/release/anvil build examples/projectile.anv -o projectile.exe
```

| Command | Does |
|---|---|
| `anvil check f.anv [--json]` | type-check |
| `anvil run f.anv` | interpret `main` |
| `anvil build f.anv [-o out.exe] [--asm]` | compile to a native executable |
| `anvil test f.anv [--json]` | run `test` blocks |
| `anvil explain [CODE]` | explain an error code |
| `anvil spec` | print the language specification |

## Layout

| Path | Contents |
|---|---|
| `crates/anvil_front` | lexer, parser, checker, diagnostics, typed IR |
| `crates/anvil_interp` | reference interpreter |
| `crates/anvil_codegen` | Cranelift backend, C runtime, linker driver |
| `crates/anvil_cli` | the `anvil` command and end-to-end tests |
| `tests/programs` | programs with golden output, run on both backends |
| `tests/errors` | one file per error code, with expected codes and fix checks |

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT) at your
option. Contributions are accepted under the same terms.
