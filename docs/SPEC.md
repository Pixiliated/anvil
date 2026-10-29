# Anvil language specification (v0.1)

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
fn name(param: Type, ...) -> Ret uses io requires cond ensures cond { body }
test "description" { body }
```

Comments start with `//` and run to the end of the line. Identifiers are ASCII letters, digits
and `_`, not starting with a digit. Keywords: `fn let var if else while for in return break
continue struct unit test requires ensures uses true false result`.

## Types

| Type | Values |
|---|---|
| `i64` | 64-bit signed integers; overflow is a panic, never wraps |
| `f64` | IEEE 754 doubles, optionally with a unit: `f64[m/s]` |
| `bool` | `true`, `false` |
| `str` | string literals: `"text"`, escapes `\n \t \" \\` |
| structs | declared with `struct` |

There are **no implicit conversions**. `i64` and `f64` never mix; `3` is an `i64` and `3.0`
an `f64`. Convert with `to_f64(i)` and `to_i64(x)` (truncates toward zero; panics on NaN,
infinity or out-of-range).

Float literals need digits on both sides of the point (`1.0`, `2.5e-3`). Integer literals may
use `_` separators (`1_000_000`).

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
var name: Type = value;   // mutable; must be reassigned somewhere
```

Every binding states its type. A name is declared once per function: no shadowing, even in
inner blocks. Parameters and loop variables are immutable. A variable or parameter that is
never read is an error unless its name starts with `_`.

Assignment: `name = value;` or `name.field.field = value;` (only `var` bindings).

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
- Arguments are evaluated left to right. Structs are passed and returned by value.

### Effects

Functions are pure unless they declare `uses io`. Only `io` exists in v0.1. `print`, and any
call to a function that uses io, requires `uses io` on the caller. Declaring `uses io` without
performing io is an error, so a signature always tells the truth.

### Contracts

```
fn safe_div(n: i64, d: i64) -> i64
    requires d != 0
    ensures result * d + n % d == n
{
    return n / d;
}
```

`requires` clauses run on entry, `ensures` clauses before returning, with `result` naming the
return value. A false clause is a panic naming the clause. Contracts must be pure. All
`requires` come before all `ensures`.

## Statements

```
if cond { } else if cond { } else { }
while cond { }
for i in start..end { }      // i64, end exclusive, both evaluated once
return value;  return;  break;  continue;
call(...);
```

Conditions must be `bool`. Nothing may follow `return`, `break` or `continue` in a block.

## Expressions

Precedence, lowest first: `||`, `&&`, comparisons (`== != < <= > >=`, not chainable),
`+ -`, `* / %`, unary `- !`, field access `.`.

- `+ - * /` on `i64` (checked: overflow and division by zero panic) and `f64` (IEEE: division
  by zero gives inf or nan). `%` is `i64` only, with the sign of the dividend.
- `==` and `!=` on `i64`, `f64`, `bool`. Ordering on `i64` and `f64`.
- `&&` and `||` short-circuit.
- Struct literal: `Body { mass: 1.0[kg], pos: 0.0[m] }`, every field once, in declaration
  order. Structs cannot contain themselves.
- No operator overloading, no implicit conversions, no truthiness.

## Built-in functions

| Call | Meaning |
|---|---|
| `print(a, b, ...)` | writes each argument, then a newline; needs `uses io` |
| `assert(cond)` | panics if `cond` is false |
| `sqrt(x)` | square root of an `f64`, halving the unit |
| `abs(x)` | absolute value of `i64` (panics on `i64` minimum) or `f64` |
| `to_f64(i)` | `i64` to dimensionless `f64` |
| `to_i64(x)` | dimensionless `f64` to `i64`, truncating |

Built-ins take positional arguments.

### Printing

- `i64`: decimal. `bool`: `true`/`false`. `str`: as is.
- **Floats**: the shortest digits that read back as the same value. Positional when the
  decimal exponent is in [-5, 17), otherwise scientific. Always contains a `.`: `3.0`, `0.1`,
  `1.0e17`, `1.5e-6`, `nan`, `inf`, `-inf`, `-0.0`.
- A float with a unit is followed by a space and its canonical unit: `9.81 m/s^2`. Aliases
  print expanded (`kg*m/s^2`).

## Programs, panics and exit codes

`main` takes no parameters and returns nothing or `i64`. Its `i64` result is the exit code and
must be in 0..=255. A panic writes `panic: <message>` to stderr and exits with code 101.
Panic messages name the source location: `integer overflow in `+` at main.anv:12:9`.

Output is byte-identical on every platform (`\n` line endings) and every run: Anvil v0.1 has
no randomness, clock or environment access.

## Tests

`test "name" { ... }` blocks run with `anvil test file.anv`. A test passes if it finishes and
fails if it panics (`assert`, a contract, overflow). Tests may declare `uses io`.

## Tooling

```
anvil check file.anv [--json]          type-check
anvil run   file.anv                   interpret (the reference semantics)
anvil build file.anv [-o out.exe]      native executable via Cranelift
anvil test  file.anv [--json]          run test blocks
anvil explain [CODE]                   long explanation of an error code
anvil spec                             print this document
```

Every error has a stable code (`E0301`), an exact range and, where one exists, fixes. A fix is
`mechanical` (safe to apply without thought) or `changes_meaning` (compiles, but may hide the
real bug: understand the intent first). `--json` gives the same information as structured data.

The interpreter and the native compiler must agree exactly: same stdout, same panic message,
same exit code. The one known difference: the interpreter panics past 10,000 nested calls,
while native code recurses until its 64 MiB stack runs out.
