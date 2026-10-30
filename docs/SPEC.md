# Anvil language specification (v0.6)

Anvil is a small, compiled, general-purpose language designed to be written mostly by AI agents
and read by humans. Its rules aim at three things: one obvious way to write each thing, code
that can be understood one file at a time, and mistakes caught before the program runs. When a
rule costs typing but prevents a bug class, Anvil chooses the typing.

This document is the whole language. It is short on purpose: an agent should be able to keep
all of it in context.

## Files and items

A program is one `.anv` file. Top-level items, in any order:

```
unit m;                       // base unit
unit N = kg*m/s^2;            // unit alias
struct Body { mass: f64[kg], pos: f64[m], }
enum Shape { Circle(radius: f64[m]), Empty, }
fn name(param: Type, ...) -> Ret uses io requires cond ensures cond { body }
test "description" { body }
allow untested "reason"       // before a fn: switches one lint off for it (see Projects)
```

Comments start with `//` and run to the end of the line. Identifiers are ASCII letters, digits
and `_`, not starting with a digit. Keywords: `allow fn let var if else while for in match
return break continue struct enum unit test requires ensures uses true false result`. The names
`Some`, `None`, `Ok` and `Err` are reserved for the built-in enums.

## Layout

Every program has one layout, the one `anvil fmt` prints, and a file in any other layout is an
error (E0110) whose mechanical fix is the formatted file. Four-space indents; one statement
per line; `{` ends the line that opens a block, and an empty block is `{}`; struct fields and
enum variants one per line, each ending in `,`; one blank line between items (consecutive
`unit` lines may stay together) and at most one between statements. A function with contracts
puts each `requires` and `ensures` on its own indented line and `{` on the next. A match arm
whose body is one simple statement stays on one line (`Empty => { return 0; }`) if it fits in
100 columns. A statement longer than 100 columns whose value is an array, a call or a struct
literal with two or more elements puts one element per line, each ending in `,`. Literals keep
their spelling (`1_000`, `2.5e-3`); units lose their spaces (`f64[m/s]`).

Comments stay where they are: on their own line before an item, statement, field, variant or
arm, or at the end of the line one ends on (after one space). A comment anywhere else, inside
an expression or a signature, has no place in the layout and is an error (E0111).

## Types

| Type | Values |
|---|---|
| `i64` | 64-bit signed integers; overflow is a panic, never wraps |
| `f64` | IEEE 754 doubles, optionally with a unit: `f64[m/s]` |
| `bool` | `true`, `false` |
| `str` | immutable UTF-8 text: `"text"`, `"x = {x}"` |
| `[T]` | growable array of `T`: `[1, 2, 3]`, `[]` |
| structs | declared with `struct` |
| enums | declared with `enum` |
| `Option[T]` | `Some(x)` or `None` |
| `Result[T, E]` | `Ok(x)` or `Err(e)` |

There are **no implicit conversions**. `i64` and `f64` never mix; `3` is an `i64` and `3.0`
an `f64`. Convert with `to_f64(i)` and `to_i64(x)` (truncates toward zero; panics on NaN,
infinity or out-of-range).

Float literals need digits on both sides of the point (`1.0`, `2.5e-3`). Integer literals may
use `_` separators (`1_000_000`).

**Everything is a value.** Assigning, passing or returning any value (including arrays and
structs holding arrays) gives the receiver its own copy; changing one copy never changes
another. Copies are cheap: storage is shared until one side is written.

A struct or enum cannot contain itself by value; put the inner values in an array
(`children: [Tree]`).

## Units

A float can carry a unit, checked at compile time and free at runtime.

```
unit m; unit s;
let d: f64[m] = 100.0[m];
let t: f64[s] = 9.58[s];
let v: f64[m/s] = d / t;      // * and / combine units
let a: f64[m/s^2] = v / t;
```

- `+`, `-` and comparisons need identical units. `*` and `/` combine them.
- A literal takes a unit in brackets: `9.81[m/s^2]`. A bare `1.0` is dimensionless.
- Unit syntax: names joined by `*` and `/`, exponents with `^` (`s^-1`, `m^2`), `1/s` for a
  pure inverse. Aliases expand to base units, so `N` and `kg*m/s^2` are the same type.
- To drop a unit, divide by a literal: `d / 1.0[m]` is a plain `f64` counting metres.
- `sqrt` halves exponents; `sqrt` of `f64[m]` is an error.

## Bindings

```
let name: Type = value;   // immutable
var name: Type = value;   // mutable; must be reassigned or changed somewhere
```

Every binding states its type. A name is declared once per function: no shadowing, even in
inner blocks or match arms. Parameters, loop variables and match bindings are immutable. A
variable or parameter that is never read is an error unless its name starts with `_`.

Assignment (only `var` bindings): `name = value;`, `name.field = value;`,
`name[i] = value;`, and any chain of these: `shapes[2].points[0].x = 1.0;`.

## Functions

```
fn area(width: f64[m], height: f64[m]) -> f64[m^2] {
    return width * height;
}
```

- Every path of a function with a return type ends in `return`.
- **Named arguments:** a call to a function with two or more parameters names every argument,
  in parameter order: `area(width: 2.0[m], height: 3.0[m])`. A one-parameter call does not
  name its argument.
- A call that returns a value cannot be used as a statement; bind the value.
- Functions may be declared in any order and may recurse.
- Arguments are evaluated left to right.

### Effects

Functions are pure unless they declare `uses io`. Only `io` exists. `print`, and any call to a
function that uses io, requires `uses io` on the caller. Declaring `uses io` without performing
io is an error, so a signature always tells the truth.

### Contracts

```
fn safe_div(n: i64, d: i64) -> i64
    requires d != 0
    ensures result * d + n % d == n
{
    return n / d;
}
```

`requires` clauses run on entry, `ensures` clauses before returning (including returns made by
`?`), with `result` naming the return value. A false clause is a panic naming the clause.
Contracts must be pure. All `requires` come before all `ensures`.

## Statements

```
if cond { } else if cond { } else { }
while cond { }
for i in start..end { }      // i64, end exclusive, both evaluated once
for x in array { }           // each element, in order
match value { Variant(field) => { } ... }
return value;  return;  break;  continue;
call(...);  array.push(x);
```

Conditions must be `bool`. Nothing may follow `return`, `break` or `continue` in a block.

`for x in array` runs over the array as it was when the loop started; changing the array
inside the loop does not change which elements the loop visits.

## Arrays

```
var xs: [i64] = [3, 1, 4];
xs.push(1);                  // append
let last: i64 = xs.pop();    // remove and return the last element
xs[0] = 10;
let n: i64 = xs.len();
let grid: [[i64]] = [[1, 2], [3, 4]];
```

- Indexing is bounds-checked: `xs[i]` with `i < 0` or `i >= len` panics with the index, the
  length and the location. `pop` on an empty array panics.
- `push` and `pop` change the array in place, so they need a `var` (or a field or element of
  one).
- `[]` needs an expected type from context (a typed `let`, a parameter, a field, a return).
- Arrays have no `==`; compare element by element.

## Text

`str` values are immutable. `+` joins two strings; `==`, `!=`, `<`, `<=`, `>`, `>=` compare
them byte by byte; `s.len()` is the length in bytes. Strings are not indexed.

**Interpolation** is the one way to build text from values: `"{name} is {age} years old"`.
Each `{...}` holds one expression of type `i64`, `f64`, `bool` or `str` and inserts its text
(the same text `print` writes). The expression must close on the same line and contain no
braces; it may contain other strings (`"{greet("Ada")}"`). Write `{{` and `}}` for literal
braces. Escapes: `\n \t \" \\`.

## Enums and match

```
enum Shape {
    Circle(radius: f64[m]),
    Rect(width: f64[m], height: f64[m]),
    Empty,
}

let s: Shape = Shape.Rect(width: 2.0[m], height: 3.0[m]);
match s {
    Circle(radius) => { print("circle {radius}"); }
    Rect(width, height: h) => { print("{width} by {h}"); }
    Empty => { }
}
```

- Variants have named fields, or none. Construct with `Enum.Variant(...)`, which follows the
  call rules: one field positional, two or more named in order. A variant without fields is
  written `Enum.Variant`.
- `match` is a statement. It lists **every variant exactly once**; there is no catch-all arm,
  so adding a variant makes every match that forgets it an error.
- A pattern lists every field of its variant in declaration order. `field` binds it under its
  own name, `field: name` binds it under another name, `field: _` ignores it.
- Enums have no `==`; use `match`.

## Option, Result and `?`

`Option[T]` has variants `Some(value: T)` and `None`. `Result[T, E]` has `Ok(value: T)` and
`Err(error: E)`. Their constructors are written without a prefix: `Some(3)`, `None`, `Ok(x)`,
`Err("bad")`. `Some(x)` takes its type from `x`; `None`, `Ok` and `Err` need an expected type
from context. Match on them like any enum: `Some(value) => { }`.

Recoverable errors are values; there are no exceptions. `e?` unwraps a success and returns a
failure early:

```
fn total(texts: [str]) -> Result[i64, str] {
    var sum: i64 = 0;
    for t in texts {
        let n: i64 = parse(t)?;     // on Err, total returns that Err at once
        sum = sum + n;
    }
    return Ok(sum);
}
```

- `?` on an `Option` needs the function to return an `Option`; on a `Result[T, E]`, a
  `Result[_, E]` with the same error type.
- `?` applies to the whole value of a `let`, `var`, assignment or `return`, never inside a
  larger expression.

## Expressions

Precedence, lowest first: `||`, `&&`, comparisons (`== != < <= > >=`, not chainable),
`+ -`, `* / %`, unary `- !`, then postfix: field `.f`, index `[i]`, method `.m()`, `?`.

- `+ - * /` on `i64` (checked: overflow and division by zero panic) and `f64` (IEEE: division
  by zero gives inf or nan). `%` is `i64` only, with the sign of the dividend. `+` also joins
  two `str`.
- `==` and `!=` on `i64`, `f64`, `bool`, `str`. Ordering on `i64`, `f64`, `str`.
- `&&` and `||` short-circuit.
- Struct literal: `Body { mass: 1.0[kg], pos: 0.0[m] }`, every field once, in declaration
  order.
- Methods exist only on built-in types: `len()` on `str` and arrays, `push(x)` and `pop()` on
  arrays. Everything else is a function.
- No operator overloading, no implicit conversions, no truthiness.

**Evaluation order** is left to right. For an assignment or `push`, the value on the right is
evaluated first, then the index expressions of the target from left to right, then the
target's bounds are checked.

## Built-in functions

| Call | Meaning |
|---|---|
| `print(x)` | writes the text of `x` (`i64`, `f64`, `bool` or `str`), then a newline; needs `uses io` |
| `assert(cond)` | panics if `cond` is false |
| `sqrt(x)` | square root of an `f64`, halving the unit |
| `abs(x)` | absolute value of `i64` (panics on `i64` minimum) or `f64` |
| `to_f64(i)` | `i64` to dimensionless `f64` |
| `to_i64(x)` | dimensionless `f64` to `i64`, truncating |

Built-ins take positional arguments. To print several values, interpolate:
`print("{a} and {b}")`.

### Text of values

- `i64`: decimal. `bool`: `true`/`false`. `str`: as is.
- **Floats**: the shortest digits that read back as the same value. Positional when the
  decimal exponent is in [-5, 17), otherwise scientific. Always contains a `.`: `3.0`, `0.1`,
  `1.0e17`, `1.5e-6`, `nan`, `inf`, `-inf`, `-0.0`.
- A float with a unit is followed by a space and its canonical unit: `9.81 m/s^2`. Aliases
  print expanded (`kg*m/s^2`).

## Programs, panics and exit codes

`main` takes no parameters and returns nothing or `i64`. Its `i64` result is the exit code and
must be in 0..=255. A panic writes `panic: <message>` to stderr and exits with code 101.
Panic messages name the source location: `index 3 out of bounds for length 3 at
main.anv:12:9`.

Output is byte-identical on every platform (`\n` line endings) and every run: Anvil has no
randomness, clock or environment access.

## Tests

`test "name" { ... }` blocks run with `anvil test file.anv`. A test passes if it finishes and
fails if it panics (`assert`, a contract, overflow, a bad index). Tests may declare `uses io`.

## Tooling

```
anvil check   file.anv                 type-check
anvil run     file.anv [--max-steps N] interpret (the reference semantics)
anvil build   file.anv [-o out.exe] [--asm]
                                       native executable via Cranelift
anvil test    file.anv [--max-steps N] run test blocks
anvil fmt     [file.anv] [--check]     put the file (or every project file) in canonical
                                       layout; --check changes nothing
anvil fix     file.anv                 apply every mechanical fix until none is left
anvil trace   file.anv [--test NAME] [--only F,G] [--limit N]
                                       a run as a timeline: calls, returns, prints
anvil profile file.anv [--test NAME]   calls and steps per function
anvil outline file.anv                 items and signatures, without reading the whole file
anvil type-at file.anv LINE COL        type and declaration at a position
anvil refs    file.anv NAME            declaration and uses of NAME (fn, type, unit,
                                       Type.field, Enum.Variant, Enum.Variant.field)
anvil explain [CODE]                   long explanation of an error code or a lint
anvil spec                             print this document
```

Every command takes `--json`. Every error has a stable code (`E0301`), an exact range and,
where one exists, fixes. A fix is `mechanical` (safe to apply without thought) or
`changes_meaning` (compiles, but may hide the real bug: understand the intent first);
`anvil fix` applies only mechanical ones (including the layout, last). A syntax error stops
checking, but the parser goes on at the next item (`fn`, `struct`, `enum`, `unit`, `test`,
`allow`), so one run reports the first syntax error of every item. `type-at` and `refs` answer even for a file with
type errors. `--max-steps` stops a run after that many statements and loop iterations.

`trace` and `profile` run `main`, or one test with `--test`, in the interpreter. A trace lists
each call with its arguments, each return value and each printed line, with the step and call
depth; values are written as Anvil source (`2.0[m]`, `Shape.Circle(1.5)`, `Some(3)`), cut
after 8 elements, 80 string bytes or 200 characters, saying how much was cut. `--only` keeps
the calls of the named functions (prints are always kept); `--limit` (default 1000) counts the
events left out. A profile gives, per function, its calls, its steps including callees and its
own steps. Both count steps, never time, so they give the same answer everywhere; a panic or
the step limit ends the run and is reported, not treated as a failure of the command.

### Projects

A directory with `anvil.toml` is a project:

```toml
[project]
name = "orders"
files = ["src/*.anv", "tools/report.anv"]   # `*` matches within one directory

[golden]
dir = "golden"                              # optional; this is the default
```

Unknown keys, patterns that match nothing and files listed twice are errors. Project commands
find `anvil.toml` in the current directory or above:

```
anvil list                                  files: functions, tests, main, golden
anvil golden check [FILE] [--native]        compare output with approved goldens
anvil golden approve FILE                   approve a program's current output
anvil verify [--native]                     check, test and golden-check every file;
                                            store the findings in the record
anvil review [--native]                     verify, then write out/review.md for sign-off
anvil record query [KIND] [--open]          milestones, decisions, findings, runs
anvil record decide TITLE [BODY]            record a decision
anvil record status MILESTONE STATUS        open, active, done or dropped
anvil record import-roadmap [FILE]          milestones from docs/ROADMAP.md
anvil record export                         the record as Markdown
anvil watch [--native] [--runs N]           verify, and again after every change
```

A **golden** (`golden/<file>.out`) is a program's approved output: its exit code, its panic
message if any, and every byte it printed. `golden check --native` also builds each program and
requires the executable to match and to free every object. Only `golden approve` writes one,
after the output has been read. The **record** (`anvil_record.db`, not checked in) keeps
milestones, decisions, and every problem `verify` finds, with when it was first seen and when it
was resolved; a problem that comes back opens a new entry, so regressions keep their history.
When a program no longer matches its golden, `golden check` writes what it printed to
`out/golden/<file>.out` and a line diff to `out/golden/<file>.diff` (`.native.out` and
`.native.diff` for the executable).

**Lints** are checks `verify` makes beyond type errors. There is one: **`untested`**, a function
that no test and no golden-checked `main` runs (`main` itself is left to its golden). Fix it with
a test; where no test can reach the function, write `allow untested "reason"` on its own line
before the `fn`. The reason is required, an unknown lint or a repeated allow is an error, and
an allow on a function that does run is reported as stale.

`anvil-mcp` serves the same commands, except `watch`, as MCP tools over stdio (`type-at` is
`type_at`, `golden check` is `golden_check`), with the same arguments and the same JSON; a
failing check is a tool error. Over MCP, every run stops after 100,000,000 steps unless
`max_steps` says otherwise.

The interpreter and the native compiler must agree exactly: same stdout, same panic message,
same exit code. The one known difference: the interpreter panics past 10,000 nested calls,
while native code recurses until its 64 MiB stack runs out.
