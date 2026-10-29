# Anvil

A small compiled programming language designed to be written mostly by AI agents and read by
people.

```
unit m;

enum Shape {
    Circle(radius: f64[m]),
    Rect(width: f64[m], height: f64[m]),
}

fn area(s: Shape) -> f64[m^2] {
    match s {
        Circle(radius) => { return 3.14159 * radius * radius; }
        Rect(width, height) => { return width * height; }
    }
}

fn parse_digit(c: str) -> Result[i64, str] {
    if c == "0" { return Ok(0); }
    if c == "1" { return Ok(1); }
    return Err("not a binary digit: {c}");
}

fn binary(digits: [str]) -> Result[i64, str] {
    var n: i64 = 0;
    for d in digits {
        let bit: i64 = parse_digit(d)?;    // returns the Err early
        n = n * 2 + bit;
    }
    return Ok(n);
}

fn main() uses io {
    let shapes: [Shape] = [Shape.Circle(1.0[m]), Shape.Rect(width: 2.0[m], height: 3.0[m])];
    for s in shapes {
        print("area {area(s)}");               // area 3.14159 m^2, then area 6.0 m^2
    }
    match binary(["1", "0", "1"]) {
        Ok(value) => { print("binary 101 is {value}"); }
        Err(error) => { print(error); }
    }
}

test "rectangles" {
    assert(area(Shape.Rect(width: 2.0[m], height: 2.0[m])) == 4.0[m^2]);
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
- **Errors as values.** `Option` and `Result` with `?`, and `match` that must handle every
  variant: there is no catch-all arm and no exception to forget.
- **Values, not references.** Arrays and structs copy on assignment (cheaply: storage is shared
  until written), so no two names ever see each other's changes.
- **Deterministic and loud.** Integer overflow panics. Output is byte-identical on every run
  and platform. The same source gives a byte-identical executable.

The whole language is in [docs/SPEC.md](docs/SPEC.md), which is short enough to give an agent
in full (`anvil spec` prints it).

## Status

v0.3: single-file programs with i64, f64 with units, bool, strings with interpolation,
growable arrays, structs, enums with exhaustive `match`, `Option` and `Result` with `?`,
contracts, effects and tests. There are two backends:

- an **interpreter**, which is the reference semantics;
- a **native compiler**: Cranelift to x86-64, linked with MSVC on Windows. Strings and arrays
  are reference counted with copy-on-write; the tests check that every object is freed.

The test suite runs every program through both and requires identical stdout, panic messages
and exit codes. A formatter, modules, generics and other platforms are on the
[roadmap](docs/ROADMAP.md).

## Using it

You need Rust (stable). Native builds also need Windows x64 with the Visual Studio C++ build
tools.

```bash
cargo build --release
```

```bash
target/release/anvil run examples/orders.anv
```

```bash
target/release/anvil build examples/orders.anv -o orders.exe
```

| Command | Does |
|---|---|
| `anvil check f.anv` | type-check |
| `anvil run f.anv [--max-steps N]` | interpret `main` |
| `anvil build f.anv [-o out.exe] [--asm]` | compile to a native executable |
| `anvil test f.anv [--max-steps N]` | run `test` blocks |
| `anvil fix f.anv` | apply every mechanical fix until none is left |
| `anvil outline f.anv` | list items and signatures |
| `anvil type-at f.anv LINE COL` | type and declaration at a position |
| `anvil refs f.anv NAME` | where a function, type, field, variant or unit is declared and used |
| `anvil explain [CODE]` | explain an error code |
| `anvil spec` | print the language specification |

Every command takes `--json`.

## For agents: the MCP server

`anvil-mcp` serves every command above as an MCP tool over stdio, with the same arguments and
the same JSON (a test keeps the two in step). A failing check is a tool error. To add it to
Claude Code:

```bash
claude mcp add anvil -- target/release/anvil-mcp --root .
```

An agent starts with `spec` (the whole language), writes a file, then loops on `check` and
`fix`; `outline`, `type_at` and `refs` let it navigate a file without reading all of it.

## Layout

| Path | Contents |
|---|---|
| `crates/anvil_front` | lexer, parser, checker, diagnostics, typed IR |
| `crates/anvil_interp` | reference interpreter |
| `crates/anvil_codegen` | Cranelift backend, C runtime, linker driver |
| `crates/anvil_ops` | every command, returning the JSON the CLI and MCP server send |
| `crates/anvil_cli` | the `anvil` command and end-to-end tests |
| `crates/anvil_mcp` | the `anvil-mcp` server, its stdio and parity tests |
| `tests/programs` | programs with golden output, run on both backends |
| `tests/errors` | one file per error code, with expected codes and fix checks |

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT) at your
option. Contributions are accepted under the same terms.
