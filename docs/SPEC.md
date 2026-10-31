# Anvil language specification (v0.11)

Anvil is a small, compiled, general-purpose language designed to be written mostly by AI agents
and read by humans. Its rules aim at three things: one obvious way to write each thing, code
that can be understood one file at a time, and mistakes caught before the program runs. When a
rule costs typing but prevents a bug class, Anvil chooses the typing.

This document is the whole language. It is short on purpose: an agent should be able to keep
all of it in context.

## Files and items

A program is one `.anv` file, its root, and the modules it imports (see Modules). Imports
come first; the other top-level items follow in any order:

```
import shapes.{area, Shape};  // names from shapes.anv next to this file
unit m;                       // base unit
unit N = kg*m/s^2;            // unit alias
struct Body { mass: f64[kg], pos: f64[m], }
enum Shape { Circle(radius: f64[m]), Empty, }
fn name(param: Type, ...) -> Ret uses io requires cond ensures cond { body }
fn first[T](xs: [T]) -> Option[T] { body }   // generic (see Generics)
test "description" { body }
allow untested "reason"       // before a fn: switches one lint off for it (see Projects)
```

Comments start with `//` and run to the end of the line. Identifiers are ASCII letters, digits
and `_`, not starting with a digit. Keywords: `allow fn import let pub var if else while for in
match return break continue struct enum unit test requires ensures uses true false result`. The names
`Some`, `None`, `Ok` and `Err` are reserved for the built-in enums.

## Modules

Every file is a module, and a file uses another's items only by importing them by name:

```
// main.anv
import lib.geometry.{Shape, area};

fn main() uses io {
    print("{area(Shape.Square(2.0))}");
}

// lib/geometry.anv
pub enum Shape {
    Square(side: f64),
}

pub fn area(s: Shape) -> f64 {
    var out: f64 = 0.0;
    match s {
        Square(side) => { out = side * side; }
    }
    return out;
}
```

- `import a.b.{x, y};` reads `a/b.anv` **relative to the importing file's directory**, so every
  file can also be checked, tested or run on its own. Names are listed; there are no wildcard
  imports. A module is read once however many files import it.
- Only items marked `pub` (`pub fn`, `pub struct`, `pub enum`, `pub unit`) can be imported. A
  pub struct shows all its fields, a pub enum all its variants. A pub item cannot use a type its
  module keeps private (E0607).
- Every imported name must be used (E0604, with a mechanical fix that removes it), and it cannot
  also be declared in the importing file. Imports form a tree: cycles are an error (E0605).
- Only the root file may define `main` (E0606). A module's tests run when it is the root:
  `anvil test lib/geometry.anv`.
- Private items of different modules may share a name. Errors and panics inside a module name
  its file. Functions from a module show as `lib.geometry.area` in traces and profiles.
- A base unit is the same unit in every module that declares or imports its name.

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
| `Map[K, V]` | keys of type `i64` or `str`, each with one value: `["a": 1]`, `[:]` |
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
let mid: [i64] = xs[1..3];   // a copy of elements 1 and 2
```

- Indexing is bounds-checked: `xs[i]` with `i < 0` or `i >= len` panics with the index, the
  length and the location. `pop` on an empty array panics.
- `push` and `pop` change the array in place, so they need a `var` (or a field or element of
  one).
- `[]` needs an expected type from context (a typed `let`, a parameter, a field, a return).
- **Slices** `xs[from..to]` copy elements `from` up to, not including, `to`. Both bounds are
  required; `from < 0`, `to > len` or `from > to` panics with the range, the length and the
  location. A slice is not a place: it cannot be assigned.

## Text

`str` values are immutable. `+` joins two strings; `==`, `!=`, `<`, `<=`, `>`, `>=` compare
them byte by byte; `s.len()` is the length in bytes. Strings are not indexed by position, but
they slice by byte offsets: `s[from..to]`, with the bounds rules of arrays, and a panic if
either bound falls inside a character (`"héllo"[0..2]`).

| Method | Gives |
|---|---|
| `s.contains(t)` | `bool` |
| `s.starts_with(t)`, `s.ends_with(t)` | `bool` |
| `s.find(t)` | `Option[i64]`: the byte offset of the first `t` (`Some(0)` for `""`) |
| `s.split(sep)` | `[str]`: the pieces between occurrences of `sep`, left to right; `"a,,b"` gives `["a", "", "b"]`, `""` gives `[""]`; an empty `sep` panics |

**Interpolation** is the one way to build text from values: `"{name} is {age} years old"`.
Each `{...}` holds one expression of type `i64`, `f64`, `bool` or `str` and inserts its text
(the same text `print` writes). The expression must close on the same line and contain no
braces; it may contain other strings (`"{greet("Ada")}"`). Write `{{` and `}}` for literal
braces. Escapes: `\n \t \" \\`.

## Maps

```
var ages: Map[str, i64] = ["ada": 36, "alan": 41];
ages.set(key: "grace", value: 85);   // insert, or replace the value
ages.remove("alan");                  // panics if the key is not there
let n: i64 = ages.len();
match ages.get("ada") {
    Some(value) => { print("ada is {value}"); }
    None => {}
}
for name in ages.keys() {
    print(name);
}
```

- Keys are `i64` or `str` (E0408), and a map always visits them **in ascending order** (`str`
  bytewise, so `"Z"` comes before `"a"`). The order never depends on history, so two maps with
  the same entries are the same value.
- `m.get(k)` is an `Option` of the value; `m.has(k)` a `bool`; `m.keys()` and `m.values()`
  arrays in key order. There is no `for` over a map itself: loop over `keys()` or `values()`.
- `set` and `remove` change the map in place, so they need a `var`; `set` names its arguments
  like any call with two parameters.
- A literal `[k: v, ...]` that repeats a key panics. `[:]` needs an expected type.
- Maps are values like arrays: assigning or passing one gives an independent copy.

## Generics

```
fn largest[T: ordered](xs: [T]) -> Option[T] { ... }
struct Pair[A, B] { first: A, second: B, }
enum Tree[T] { Leaf(value: T), Node(children: [Tree[T]]), }

let p: Pair[i64, str] = Pair { first: 1, second: "one" };
let top: Option[i64] = largest([4, 9, 2]);
let leaf: Tree[str] = Tree.Leaf("a");
```

- Functions, structs and enums take type parameters in brackets after their name. Written
  types give the arguments (`Pair[i64, str]`), but calls, literals and constructors never do:
  the arguments come from the values passed (or from the expected type). So every type
  parameter appears in a parameter's or a field's type (E0700).
- A type parameter allows copying, passing, storing and `==`. Anything more is a constraint
  declared with it, so the signature tells every caller what the body needs:

| Constraint | Types | Allows |
|---|---|---|
| `ordered` | i64, f64, str | `<`, `<=`, `>`, `>=` |
| `text` | i64, f64, bool, str | print, interpolation |
| `key` | i64, str | map keys |

  `T: ordered + text` combines them. An operation the constraints do not allow is E0703 (its
  fix adds the constraint); a call, literal or type whose arguments do not meet them is E0701,
  reported where it is used.
- A generic body is checked once, against its constraints. Each set of type arguments then
  gets its own copy (`max[i64]`, `Pair[i64, str]` in traces and errors), so generic code is as
  fast as code written for one type. A generic that calls itself with a larger type
  (`f[T]` calling `f[[T]]`) would need endless copies (E0704).
- Generics can be pub and imported like any item.

## Equality

`==` and `!=` work on any two values of the same type. Arrays, structs, enums and maps are equal
when every part is: the same length, fields, variant and payload, keys and values. Floats
compare as IEEE values everywhere, so `[nan] != [nan]` and `[0.0] == [-0.0]`. Only `i64`, `f64`
and `str` are ordered.

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
- Enums have `==` (see Equality); read their fields with `match`.

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
- `==` and `!=` on any two values of one type (see Equality). Ordering on `i64`, `f64`, `str`.
- `&&` and `||` short-circuit.
- Struct literal: `Body { mass: 1.0[kg], pos: 0.0[m] }`, every field once, in declaration
  order.
- Methods exist only on built-in types: `len()` on `str`, arrays and maps; `push(x)` and
  `pop()` on arrays; the search methods on `str` (see Text); `get`, `has`, `set`, `remove`,
  `keys` and `values` on maps. Everything else is a function.
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

A **property test** names inputs after `for all` and runs on generated values of their types:

```
test "sort keeps the length" for all xs: [i64] {
    assert(sort(xs).len() == xs.len());
}
```

It runs 100 cases (`--cases N`), small inputs first. The inputs come from a generator seeded by
the test's name (and `--seed N`, default 0), so every run sees the same cases. Any type works:
integers include 0, -1 and the extremes, floats are finite, text includes characters of two and
three bytes, and arrays, maps, structs and enums are built from their parts. A failing case is
shrunk while it still fails, and reported with its smallest inputs: `for all: xs: [-1]`.

`anvil test --native` also builds the plain tests into one executable and runs each natively,
with the leak check; a test that passes, panics or prints differently there fails as "the
backends disagree". Property tests run in the interpreter, the reference.

## Tooling

```
anvil check   file.anv                 type-check
anvil run     file.anv [--max-steps N] interpret (the reference semantics)
anvil build   file.anv [-o out] [--asm]
                                       native executable via Cranelift, for the host:
                                       Windows x64 (MSVC), Linux or macOS (x86-64, ARM64)
anvil test    file.anv [--native] [--cases N] [--seed N] [--max-steps N]
                                       run test blocks; `for all` tests on generated inputs
anvil fmt     [file.anv] [--check]     put the file (or every project file) in canonical
                                       layout; --check changes nothing
anvil fix     file.anv                 apply every mechanical fix until none is left
anvil fuzz    [--seed N] [--count N]   random programs on both backends (see below)
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
`anvil fix` applies only mechanical ones (including the layout, last), and only in the file it
was given; errors in imported modules are reported with their own file. A syntax error stops
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
same exit code. `anvil fuzz` checks it on random well-typed programs (seed N always makes the
same program); a program on which they differ is saved to `out/fuzz/<seed>.anv`. The one known difference: the interpreter panics past 10,000 nested calls,
while native code recurses until its 64 MiB stack runs out.
