# Anvil language specification (v0.40)

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
const MOST: i64 = 512;        // a constant (see Bindings)
struct Body { mass: f64[kg], pos: f64[m], }
enum Shape { Circle(radius: f64[m]), Empty, }
fn name(param: Type, ...) -> Ret uses io noalloc requires cond ensures cond { body }
fn first[T](xs: [T]) -> Option[T] { body }   // generic (see Generics)
fn Body.speed(self) -> f64[m/s] { body }     // a method (see Methods and interfaces)
interface Moving { fn speed(self) -> f64[m/s]; }
extern "lib/fast.c" fn mix(a: i64, b: i64) -> i64;   // written in C (see C functions)
test "description" { body }
allow untested "reason"       // before a fn: switches one lint off for it (see Projects)
```

Comments start with `//` and run to the end of the line (see Doc comments below). Identifiers are ASCII letters, digits
and `_`, not starting with a digit. Keywords: `allow const fn import let pub var if else while for in
match return break continue struct enum unit test requires ensures uses noalloc extern interface
self true false result`. The names
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

### The standard library

`import std.list.{map, sort_by};` imports from the standard library: modules written in Anvil
and built into the compiler, the same from every file (so `std` is not a name for a module of
your own). `anvil std` lists them with their pub items; `anvil std list` prints one's source.

| Module | Has |
|---|---|
| `std.expect` | `expect(o: option, why: text)` and `expect_ok(r: result, why: text)`: the value, or a panic with `why` |
| `std.list` | `map`, `keep`, `fold`, `any`, `all`, `count`, `first_where`, `index_of`, `contains`, `sort_by` (stable), `sort_by_key`, `min_by_key`, `max_by_key`, `sum`, `range`, `concat`, `flatten`, `unique`, `chunks` |
| `std.set` | `Set[T: key]` with `has`, `add`, `remove`, `len`, `items`, `union`, `intersection`, `difference`; `set_of` |
| `std.text` | `pad_left`, `pad_right`, `repeat`, `lines`, `words` |
| `std.math` | `min`, `max`, `clamp`, `pow`, `gcd`, `sign`, `floor`, `ceil`, `round` (halves to even) |

The functions they take are pure (`fn(T) -> U`). Their functions show as `std.list.map` in
traces, and they have tests of their own like any module.

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
| `bytes` | immutable bytes, each 0..=255 (see Bytes) |
| `[T]` | growable array of `T`: `[1, 2, 3]`, `[]` |
| `Map[K, V]` | keys of type `i64` or `str`, each with one value: `["a": 1]`, `[:]` |
| structs | declared with `struct` |
| enums | declared with `enum` |
| `Option[T]` | `Some(x)` or `None` |
| `Result[T, E]` | `Ok(x)` or `Err(e)` |
| `fn(T, U) -> R` | a function value (see Function values) |

There are **no implicit conversions**. `i64` and `f64` never mix; `3` is an `i64` and `3.0`
an `f64`. Convert with `to_f64(i)` and `to_i64(x)` (truncates toward zero; panics on NaN,
infinity or out-of-range).

Float literals need digits on both sides of the point (`1.0`, `2.5e-3`). Integer literals may
use `_` separators (`1_000_000`). `0xff` and `0b1010` write an integer's bits in hex or binary,
all 64 of them at most, so `0xffff_ffff_ffff_ffff` is -1 and `0x8000_0000_0000_0000` the
minimum. An integer literal takes methods: `6.xor(3)`.

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

Every binding states its type. No shadowing: a name cannot be declared while it is in scope, even
in an inner block. Blocks side by side, such as match arms or two loops, can each use a name.
Parameters, loop variables and match bindings are immutable. A
variable or parameter that is never read is an error unless its name starts with `_`.

Assignment (only `var` bindings): `name = value;`, `name.field = value;`,
`name[i] = value;`, and any chain of these: `shapes[2].points[0].x = 1.0;`.

### Constants

`const NAME: Type = value;` at the top level names a value of type `i64`, `f64`, `bool` or `str`.
The value is made of literals, other constants (in any order) and operators, and is worked out when
the program is checked, so it cannot overflow, divide by zero or depend on itself (E0351). Each use
of the name is that value, in any function, contract or test. `pub const` is imported like any other
item. A constant that nothing reads is an error (E0340), and no local may take its name (E0202).
A value that needs a call, or an array or a map, is a function that returns it. `anvil mutate`
mutates constants as it does the literals in functions, against every run.

### Moves

Values are values: passing, storing or returning one never lets a change show through
another name. Underneath, arrays, maps, text and closures are shared until written, and a
write to a shared one copies it first. The **last read** of a variable does not share: the
value **moves** to where it goes, so the receiver may hold the only reference and change it
in place. A read moves when its statement (`let`, an assignment, an expression statement,
`return`, the condition of an `if`, what a `match` or `for` is over) names the variable once and
nothing reads the variable again before it is next written, on any path through loops and
branches; what an `ensures` clause reads never moves. An argument of a call that is moved or taken
by fields is evaluated after the call's other arguments, so they may read it:
`xs = add(xs: xs, x: xs.len())` hands `xs` over.
An element of an array nothing else holds is handed to the variable of a `for` over it, so
`for x in xs` with `xs` read for the last time gives each `x` as the only reference.
So a functional update is as cheap as changing the value directly:

```
fn add(xs: [i64], x: i64) -> [i64] {
    var ys: [i64] = xs;   // moves: the caller's array, if it was handed over
    ys.push(x);           // in place
    return ys;
}

for i in 0..n {
    xs = add(xs: xs, x: i);   // linear: xs is handed over and given back
}
```

A variable also **lets go** of its value as soon as nothing can read it again: after the statement
that read it last, at the start of a branch that does not read it, after a loop that did. So what
it holds is not kept until the function ends, and a value a dead variable held is no longer shared
with it (a copy never read costs nothing). This is not a step, and only `ensures` clauses keep a
value longer. What a `match` or `?` was over goes too once the payload is out, and what a `for`
was over after its loop.

The last read of a **place** (a field or an element) moves the same way when nothing else in
the statement reads that place, a place around it or one inside it, and either the statement
writes the place again or the variable holding it is not read again and the place is reached
by fields alone:

```
bag.items = add(xs: bag.items, x: 1);                  // the field is handed over and back
rows[i] = add(xs: rows[i], x: 1);                      // the element too (the same `i`)
b = Bag { items: add(xs: b.items, x: 1), n: b.n + 1 }; // each field of the old `b`
```

Indices are the same element only when they are the same variable or literal: `rows[i]` read
while `rows[j]` is written is shared, as is `bag.items` read twice in the statement.

Keep a copy of `xs` (or read it after the call) and each call copies it, as value semantics
require. Moves change no result, only what is copied: `profile` shows the bytes copied per
function, and the byte model counts what both backends do.

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

### Function values

```
fn apply(f: fn(i64) -> i64, x: i64) -> i64 {
    return f(x);
}

fn adder(k: i64) -> (fn(i64) -> i64) {
    return fn(x: i64) -> i64 {
        return x + k;
    };
}

let a: i64 = apply(f: double, x: 21);          // a declared function as a value
let b: i64 = apply(f: adder(1), x: 41);        // a closure made by a function literal
```

- **A function type** is `fn(T, U) -> R uses io noalloc`: the parameter types, the result (none
  for a function that returns nothing), its effects and its promise. A function type written
  after `->` is in parentheses, so `uses` after it belongs to the outer function:
  `fn make() -> (fn(i64) -> i64) uses io`.
- **A declared function's name is a value** of its function type, from this file or imported.
  A generic function or a built-in is not (E0412): pass a literal that calls it.
- **A function literal** is `fn(x: i64) -> i64 { ... }`: parameters with names and types, a
  result, effects and `noalloc` like a declaration, and a body; no contracts. It can be written
  wherever a value can.
- **Captures are copies.** A literal's body may read the locals of the functions around it; each
  one it reads is copied into the closure when the literal is evaluated, so assigning the
  variable afterwards does not change the closure, and the body cannot assign it (E0410). Its
  own names must differ from the ones around it.
- **Calling a function value:** a local holding one is called by its name, with the arguments in
  order and unnamed, since a function type's parameters have no names (`f(x)`, E0304). To call
  one held in a field or an array element, bind it to a local first.
- **Effects and noalloc go with the type.** Calling a value of type `fn(str) uses io` needs
  `uses io`; a noalloc function may only call function values whose type is noalloc. A function
  that does less than a type allows fits it: a pure function can be passed for
  `fn(i64) uses io`, and a noalloc one for a type without the promise; one that does more cannot
  (E0300).
- Function values cannot be compared (E0411), printed or used as map keys, and a property test
  cannot generate them (E0414). Values holding them (a struct with a function field) cannot be
  compared either.
- A literal that captures values allocates its closure (E0333 in a noalloc function); one that
  captures nothing, and a declared function, do not. Traces show a function value by name:
  `fn double`, or `fn main/fn@12:9` for the literal at line 12, column 9 of `main`.

### Effects

Functions are pure unless they declare their effects, so a signature says everything a call can
do. There are four:

| Effect | Performed by |
|---|---|
| `io` | `print`, `print_raw`, `eprint`, `exit`, `args`, `env`, `read_line`, `read_stdin` |
| `fs` | `read_file`, `write_file`, `read_bytes`, `write_bytes`, `list_dir` |
| `clock` | `clock_ns` |
| `random` | `random_below`, `random_f64`, `random_seed` |

`fn main() uses io, random { ... }`: calling a function that has an effect needs the effect on
the caller too. Declaring an effect a function never uses is an error, so a function without
effects gives the same result for the same arguments. `uses` lists them in the order above.

- **The clock** is monotonic: `clock_ns()` is nanoseconds since the program started, never the
  date. `anvil run` and built executables read the real clock. Tests, goldens, `trace`,
  `profile` and fuzzing read a **virtual clock** whose n-th read (from 0) gives n milliseconds,
  so they stay deterministic; an executable reads it when `ANVIL_CLOCK=virtual` is set (any other
  value is an error).
- **Random numbers** come from one sequence per run (SplitMix64), starting from seed 0 every run
  and every test, so a program that never calls `random_seed` draws the same numbers each time.
  `random_seed(n)` restarts the sequence; seed it from the clock for numbers that differ between
  runs. `random_below(n)` is uniform in `0..n` and panics unless `n > 0`; `random_f64()` is uniform
  in [0, 1), a multiple of 2^-53. The sequence is part of the language: both backends, and every
  version, draw the same numbers from the same seed.

**`noalloc`** after the effects promises that a function never allocates, so it can run where
allocation is too slow or must not fail. The checker holds it to that (E0333): interpolation,
joining strings or bytes, array and map literals, slices, `split`, `trim`, `keys`, `values`,
`push`, `pop`, `set`, `remove`, `args`, `read_line`, `read_file`, `read_bytes`, `bytes_of`,
`to_bytes`, `to_str`, `hex`, `to_array`, `replace`, `to_upper`, `to_lower`, `chars`, `join`,
`fixed`, `sorted`, `reversed`, `parallel_map`, `env`, `list_dir` and assigning to an array
element may allocate;
arithmetic, bit operations, comparisons, struct fields, indexing, `len`, the str searches,
`parse_i64`, `parse_f64`, `get`, `has`, `print`, `write_file`, `write_bytes` and the other
built-ins (`eprint` and `exit` among them) never do. A
noalloc function may only call functions that are noalloc too (E0334). Allocation is not an
effect: most functions allocate, so it is the promise not to that a signature carries.

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

A last clause `steps EXPR` bounds the **work** of a call: an `i64` of the parameters (no
`result`), pure, such as `steps 2 * xs.len() + 3`. A step is what `profile` and `trace` count:
one statement or one loop iteration, callees included, so the bound is exact and the same on
every machine. It is a promise checked by `verify`, not at run time: native code does not count
steps, and both backends must agree. `steps` is a name everywhere else (`var steps: i64` is
fine); only after a signature, as the last clause, does it start one.

```
pub fn sum(xs: [i64]) -> i64
    steps 2 * xs.len() + 3
{
```

A clause `allocs EXPR` bounds the **memory** a call allocates, in bytes, the same way (after
`requires` and `ensures`, with `steps` in either order; `allocs` is a name everywhere else).
Bytes are counted by a fixed model, exact and the same on every machine, which tracks what the
native runtime copies:

| What | Bytes |
|---|---|
| a new str or bytes (concatenation, interpolation, a slice, `replace`, `join`, ...) | its length |
| a new array (a literal, a slice, `sorted`, `keys`, ...) | 8 per element |
| a new map | 16 per entry |
| `push`; `set` of a new key | 8; 16 |
| writing into an array or map shared with another value (value semantics copy it first; a variable, field or element read for the last time is not shared, see Moves) | 8 or 16 per element copied |
| a function literal that captures values | 8 per value |
| `split`, `chars`, `args`, `list_dir` | the array and each new str in it |
| appending to text that alone holds its room (`s = s + x`, or text handed over, see Building text) | x's length, and all of the text when its room doubles |
| appending to text without room of its own (shared, static, or never appended to) | all of the text and x: it is copied into new room first |

Literal text is static and costs nothing; Options, Results and structs are held inline, so only
what they hold counts. `s = "-" + s` in a loop copies all of `s` each round, so its bytes grow
with the square of the length: `cost` shows it, and `allocs` makes the fix stick.

A contract is also checked without any test: `verify` calls each pure function that has one on
generated arguments (the `contract` lint, under Projects).

**`anvil cost FILE`** measures bounds instead of guessing them, for steps and for bytes alike. Each pure function of the file
is called on 200 generated argument lists of growing size (arrays, maps and text up to 40
long, integers up to 400 and the edges of i64), and every call's steps are counted. The counts
are fitted as constant, or logarithmic, linear or quadratic in one size (`xs.len()`, an integer
parameter `n`, `abs(n)` when it may be negative, or the same of a field of a struct parameter:
`h.keys.len()`), logarithmic being `n.bit_len()` and quadratic including `n * (n + 1) / 2`, the sum
of copying everything so far once a round: the simplest form that fits about as tightly as the
best. Calls that `requires` excludes are tried again with arrays of one length and small numbers.
`--only` takes a function's name, which for a generic means each of its instances, or one instance
(`--only 'push[i64, str]'`). Bounds are reported, not held to, while measuring, so a bound that does
not hold is shown with how far it is exceeded. It prints the growth, the most steps at each size, and the tightest
`steps` clause that holds for every call measured, ready to paste; a loose fit says the steps
depend on more than one size or grow faster than quadratic. For a function with a bound, it
says how much of the bound the costliest call used, and whether the steps outgrow it. Its `requires` say which inputs the
function takes, so a requires is the fix when the function is right but its inputs are not
all good ones.

### Doc comments

The `//` lines right above an item, field or variant (above its `allow` lines), with no blank
line between, are its **documentation**. `outline`, `std`, `type-at`, completion, hover and the
docs site show it with the signature, so whoever uses an item reads its doc, not its source.

```
// The greatest common divisor, never negative; gcd(0, 0) is 0.
// example: gcd(a: 84, b: -36) == 12
pub fn gcd(a: i64, b: i64) -> i64 {
```

A doc line `// example: EXPR` above a function is also a **test** of the file, named
`example of gcd on line 2`: `EXPR` must be a pure bool expression that is true. It is checked like
any code (an error in it is reported at its place in the comment), runs with the file's tests
in both backends, and counts for coverage and mutation testing, so the examples a reader is
shown cannot go stale.

### C functions

```
extern fn atan2(y: f64, x: f64) -> f64 noalloc;          // from the C runtime (libm)
extern "c/geo.c" fn gcd(a: i64, b: i64) -> i64 noalloc;  // from a C file next to this one
```

- An `extern` declares a function written in C: a signature ending in `;`. With a file in quotes
  (relative to the Anvil file's directory, like imports) the C function is in that `.c` file;
  without one it comes from the C runtime.
- Only `i64` (C's `int64_t`) and `f64` (C's `double`, with any unit) cross, as parameters and as
  the result; an extern may return nothing (E0800). Externs cannot be generic or have contracts:
  wrap one in an Anvil function that checks its arguments.
- Anvil cannot see inside C, so an extern's effects and `noalloc` are what its declaration says,
  and the declaration is trusted. It may declare `uses clock` and `uses random`, but the virtual
  clock and the seed do not reach into C. It may not declare `uses io` or `uses fs` (E0802): C's
  output would bypass Anvil's, which tests, goldens and tools capture, and its writes would reach
  the disk where checked runs keep theirs in memory.
- They are called like any function (named arguments when there are two or more) and appear in
  traces and profiles. The C file is compiled with the platform's C compiler (MSVC's `cl.exe`, or
  `cc`) for `build`, and into a library the interpreter loads for `run`, `test` and goldens, with
  the same flags, so both backends call the same code; executables stay byte-identical. A C file
  that does not compile is an error with the compiler's message. `main` and names starting with
  `anvil_` are the runtime's (E0803).

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

**`let x: T = e else { ... };`** (or `var`) unwraps: `e` is an `Option[T]` or a `Result[T, E]`, `x`
is the value in it, and otherwise the block runs, which must leave by `return`, `break`, `continue`,
`exit` or `panic` (E0352). `else why { ... }` names a Result's error in the block:

```
let at: i64 = text.find(":") else { return None; };
let n: i64 = parse(word) else why { return Err("line {line}: {why}"); };
```

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
- `xs.reversed()` is a reversed copy. `xs.sorted()`, on `[i64]`, `[f64]` and `[str]`, is a
  sorted copy, ascending (str bytewise) and stable, so equal values keep their order (`0.0` and
  `-0.0` too); a NaN panics. To sort by anything else, write the sort with a function value.

### Parallel work

`xs.parallel_map(f)` is `[f(xs[0]), f(xs[1]), ...]`: the same array a loop calling `f` on each
element in order would build, with the work spread over the machine's processors.

```
let steps: [i64] = numbers.parallel_map(collatz);
let shouted: [str] = words.parallel_map(fn(w: str) -> str { return w.to_upper(); });
```

- `f` is a function value of type `fn(T) -> U` for `xs: [T]`, and must be **pure** (E0416): with
  no effects there is nothing the order of the work could change, so the result is the same on
  every run and every machine, and the same as the interpreter's (which runs the loop).
- If `f` panics on some elements, the program panics with the panic of the **first** of them,
  as the loop would: elements after it may have run, but nothing they did can be seen.
- Elements and results can be of any type, `f` can capture values, and `f` can itself use
  `parallel_map`. Native code gives every worker its own copies of `f` and of its share of
  `xs`, so workers share nothing.
- Native executables start at most one worker per processor (at most 64); the environment
  variable `ANVIL_THREADS` (1 to 64) sets the number instead. Anything else there is a runtime
  error.

## Text

A **raw string** is written in backticks: `` `{"a": 1}\n` `` is exactly those characters, with no
escapes, no interpolation, and line breaks kept (a CRLF as `\n`); it cannot hold a backtick. Other
strings stay on one line.

`str` values are immutable. `+` joins two strings; `==`, `!=`, `<`, `<=`, `>`, `>=` compare
them byte by byte; `s.len()` is the length in bytes. Strings are not indexed by position, but
they slice by byte offsets: `s[from..to]`, with the bounds rules of arrays, and a panic if
either bound falls inside a character (`"héllo"[0..2]`).

| Method | Gives |
|---|---|
| `s.contains(t)` | `bool` |
| `s.starts_with(t)`, `s.ends_with(t)` | `bool` |
| `s.find(t)` | `Option[i64]`: the byte offset of the first `t` (`Some(0)` for `""`) |
| `s.find_from(t: x, at: i)` | the same, from byte `i` on, so a scan is one pass; panics when `i` is outside `0..=s.len()` |
| `s.split(sep)` | `[str]`: the pieces between occurrences of `sep`, left to right; `"a,,b"` gives `["a", "", "b"]`, `""` gives `[""]`; an empty `sep` panics |
| `s.trim()` | `str` without the spaces, tabs, `\r` and `\n` at either end |
| `s.parse_i64()` | `Option[i64]`: `Some` for an optional `-` and one or more ASCII digits in range (`"007"` is 7); nothing else, so no `+`, `_` or spaces |
| `s.replace(old: a, new: b)` | `str`: every `a`, left to right, as `b`; an empty `a` panics |
| `s.to_upper()`, `s.to_lower()` | `str` with the ASCII letters changed; other characters stay |
| `s.chars()` | `[str]`: each character as a str of its own |
| `xs.join(sep)` | on `[str]`: the parts with `sep` between them |
| `x.fixed(n)` | on `f64`: the nearest decimal with `n` digits after the point (0..=17, else a panic; ties go to even), without the unit: `3.14159.fixed(2)` is `"3.14"`; `nan`, `inf` and `-inf` as print writes them |
| `s.parse_f64()` | `Option[f64]`: `Some` for an optional `-`, digits, optionally `.` and digits, and optionally `e` or `E`, a sign and digits (`"-2.5e3"`), read as the nearest double, which must be finite; not `.5`, `1.`, `inf` or `nan`. Every float's text reads back as that float |

**Interpolation** is the one way to build text from values: `"{name} is {age} years old"`.
Each `{...}` holds one expression of type `i64`, `f64`, `bool` or `str` and inserts its text
(the same text `print` writes). The expression must close on the same line and contain no
braces; it may contain other strings (`"{greet("Ada")}"`). Write `{{` and `}}` for literal
braces. Escapes: `\n \r \t \" \\`.

### Building text

`s = s + x`, or a chain `s = s + a + b + ...`, on a str or bytes local, when no part reads `s`
itself, **appends in place**: while `s` holds the only reference to its text, the text grows into
room that doubles (a power of two above its length, at least 16 bytes), so building text this way
takes time and memory linear in its length, in both backends. Text anything else shares (a copy
kept in another variable, an array or a struct, or a literal) is copied first, so no other value
ever changes; a part that reads `s` (`s = s + s`) is evaluated with the old text, as everywhere
else. Prepending (`s = x + s`), or interpolating `s` into itself, copies: build a list of parts
and `join` them instead.

Text **handed over** appends the same way: `a + b` where `a` is a variable, field or element read
for the last time (see Moves), what a call returned, or itself such an append, adds `b` to `a`'s
text. So `return s + "!"` in a function handed `s`, `p.name = p.name + x`, `f(s) + x` and
`s + a + b` on a moved `s` grow the text in place too. Only text made by an append has room; the first append to other
text copies it into room of its own.

## Bytes

`bytes` are immutable bytes of any value, for binary data and hashing; `str` is bytes that are
UTF-8.

| Operation | Gives |
|---|---|
| `bytes_of(xs)` | the `[i64]` as bytes; panics on a value outside 0..=255 |
| `s.to_bytes()` | a `str`'s UTF-8 bytes |
| `b.len()`, `b[i]` | the length; byte `i` as an `i64` 0..=255 (bounds-checked) |
| `b[from..to]`, `a + b` | a slice; the two joined |
| `a == b`, `a != b` | same bytes |
| `b.to_str()` | `Option[str]`: `Some` when the bytes are UTF-8 |
| `b.hex()` | two lowercase hex digits per byte: `"00ff"` |
| `b.to_array()` | each byte as an `i64` |
| `read_bytes(path)` | `Result[bytes, FileError]`: the whole file, any bytes; needs `uses fs` |
| `write_bytes(path: p, data: d)` | like `write_file`; needs `uses fs` |

Bytes cannot be changed in place (E0310) or printed: print `b.hex()`. Traces show them as
`bytes_of([104, 105])`, and property tests generate them.

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

  An interface is a constraint too (see Methods and interfaces). `T: ordered + text` combines
  them. An operation the constraints do not allow is E0703 (its
  fix adds the constraint); a call, literal or type whose arguments do not meet them is E0701,
  reported where it is used.
- A generic body is checked once, against its constraints. Each set of type arguments then
  gets its own copy (`max[i64]`, `Pair[i64, str]` in traces and errors), so generic code is as
  fast as code written for one type. A generic that calls itself with a larger type
  (`f[T]` calling `f[[T]]`) would need endless copies (E0704).
- Generics can be pub and imported like any item.

## Methods and interfaces

```
struct Circle { r: f64[m], }

fn Circle.area(self) -> f64[m^2] {
    return 3.0 * self.r * self.r;
}

fn Circle.scaled(self, by: f64) -> Circle {
    return Circle { r: self.r * by };
}

interface Shape {
    fn area(self) -> f64[m^2];
    fn scaled(self, by: f64) -> Self;
}

fn doubled_area[T: Shape](x: T) -> f64[m^2] {
    return x.scaled(2.0).area();
}

let a: f64[m^2] = Circle { r: 1.0[m] }.area();
```

- **A method** is a function declared on a struct or enum, `fn Type.name(self, ...)`, in the
  module that declares the type (E0415; built-in types take none). `self` is the value it is
  called on, immutable like any parameter, and may go unused. A generic type's methods name its
  parameters: `fn Stack[T].push(self, x: T) -> Stack[T]`; a method may have type parameters of
  its own after its name: `fn Circle.measure[U](self, f: fn(f64[m]) -> U) -> U`.
- **Calls** are `x.name(args)`: the arguments after `self` follow the rule for calls (one is
  unnamed, two or more are named). A value's methods are found by its type, so they need no
  import; another module's methods are callable when they are `pub` (E0603). A method cannot
  share a name with a field or variant of its type (E0201). Methods are plain functions:
  values are passed in, a changed value is returned (`s = s.push(1)`), and traces name them
  `Circle.area` (`Stack.push[i64]` for a generic type's).
- **An interface** lists method signatures, with `self` first and `Self` for the type that has
  them. It is a constraint on type parameters, alone or with others (`T: Shape + text`); a
  generic body may call its methods on values of `T`. A type meets it when it has every method
  with the same parameter and result types, `Self` being the type; its effects must be among
  the interface method's, and it must be `noalloc` where the interface's is (E0701 says what
  differs). Nothing declares that a type implements an interface: having the methods is enough.
- Interfaces are constraints, not types: a list of different shapes is an enum whose variants
  hold them. They can be `pub` and imported like types.

## Equality

`==` and `!=` work on any two values of the same type, except functions and values that hold
them (E0411). Arrays, structs, enums and maps are equal
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
  so adding a variant makes every match that forgets it an error. One arm can name several:
  `Red | Green => { ... }`, with their fields ignored and nothing bound. A variant named alone,
  `Circle => { ... }`, ignores its fields likewise.
- A pattern lists every field of its variant in declaration order. `field` binds it under its
  own name, `field: name` binds it under another name, `field: _` ignores it. The only field of
  a variant takes any name: `Some(x)`, `Err(_)`.
- Enums have `==` (see Equality); read their fields with `match`.

## Option, Result and `?`

`Option[T]` has variants `Some(value: T)` and `None`. `Result[T, E]` has `Ok(value: T)` and
`Err(error: E)`. Their constructors are written without a prefix: `Some(3)`, `None`, `Ok(x)`,
`Err("bad")`. `Some(x)` takes its type from `x`; `None`, `Ok` and `Err` need an expected type
from context. Match on them like any enum: `Some(x) => { }`, `Err(why) => { }`.

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

Precedence, lowest first: `||`, `&&`, comparisons (`== != < <= > >=`, not chainable), `|`,
`&`, `<< >>`, `+ -`, `* / %`, unary `- ! ~`, then postfix: field `.f`, index `[i]`, method
`.m()`, `?`. So `x & mask == 0` compares the masked value and `1 + 2 << 3` is 24, as in C and
Rust.

- `+ - * /` on `i64` (checked: overflow and division by zero panic) and `f64` (IEEE: division
  by zero gives inf or nan). `%` is `i64` only, with the sign of the dividend. `+` also joins
  two `str`.
- `==` and `!=` on any two values of one type (see Equality). Ordering on `i64`, `f64`, `str`.
- `&&` and `||` short-circuit.
- **Bits** of `i64`: `a & b`, `a | b`, `~a`, `a << n` and `a >> n` (which keeps the sign);
  methods `a.xor(b)` and `a.unsigned_shr(n)` (which fills with zeros). A shift amount outside
  0..=63 panics; bits shifted out are dropped. `^` is not an operator (E0393): it is a power in
  units, and `x ^ 2` would be read both ways.
- **Wrapping arithmetic**, for hashes and generators: `a.wrapping_add(b)`, `a.wrapping_sub(b)`
  and `a.wrapping_mul(b)` wrap around at the ends of i64 instead of panicking.
- `a.bit_len()` is the number of bits it takes to write `a` taken as unsigned: 0 for 0, 8 for 255,
  64 for a negative. It is what a logarithmic `steps` bound is written with.
- `if c { a } else { b }` is a value, of the type both branches have, each branch one expression;
  only the chosen branch runs, and it counts as a branch for the `unreached` lint. `else if` chains.
  At the start of a statement `if` is the statement.
- A **character literal** is the number of one ASCII character, an i64: `'*'` is 42, `'\n'` is 10.
  The escapes are `\n \r \t \\ \'`. Compare it with the elements of `bytes`.
- Struct literal: `Body { mass: 1.0[kg], pos: 0.0[m] }`, every field once, in declaration
  order.
- Built-in types have built-in methods: `len()` on `str`, `bytes`, arrays and maps; `push(x)`
  and `pop()` on arrays; the str methods (see Text); `get`, `has`, `set`, `remove`, `keys` and
  `values` on maps; the bit and wrapping methods on `i64`; the bytes methods (see Bytes).
  Structs and enums have the methods declared for them (see Methods and interfaces).
- No operator overloading, no implicit conversions, no truthiness.

**Evaluation order** is left to right. For an assignment or `push`, the value on the right is
evaluated first, then the index expressions of the target from left to right, then the
target's bounds are checked.

## Built-in functions

| Call | Meaning |
|---|---|
| `print(x)` | writes the text of `x` (`i64`, `f64`, `bool` or `str`), then a newline; needs `uses io` |
| `print_raw(text)` | writes the `str` with no newline; needs `uses io` |
| `eprint(x)` | the same as `print`, to stderr; needs `uses io` |
| `panic(message)` | stops the program with `panic: <message> (<where>)`. It ends a path like `return`, and a branch that ends in it needs no test |
| `exit(code)` | ends the program with the code (0..=255, else a panic); nothing after it runs, so it ends a function's path like `return`; in a test it is a failure; needs `uses io` |
| `env(name)` | `Option[str]`: the environment variable; names starting `ANVIL_` are the runtime's and read as `None`; needs `uses io` |
| `list_dir(path)` | `Result[[str], FileError]`: the names in the directory, sorted bytewise; needs `uses fs` |
| `assert(cond)` | panics if `cond` is false. A comparison of numbers, booleans or text also shows both sides, text quoted and, when long, cut to where the sides first differ. In a test, a comparison of anything else is written out beside the panic, and two arrays say where they part |
| `sqrt(x)` | square root of an `f64`, halving the unit |
| `abs(x)` | absolute value of `i64` (panics on `i64` minimum) or `f64` |
| `to_f64(i)` | `i64` to dimensionless `f64` |
| `to_i64(x)` | dimensionless `f64` to `i64`, truncating |
| `clock_ns()` | `i64` nanoseconds since the program started; needs `uses clock` |
| `random_below(n)` | uniform `i64` in `0..n`, panics unless `n > 0`; needs `uses random` |
| `random_f64()` | uniform `f64` in [0, 1); needs `uses random` |
| `random_seed(n)` | restarts the random sequence from seed `n`; needs `uses random` |
| `args()` | `[str]`: the program's command-line arguments, without its own name; needs `uses io` |
| `read_line()` | `Option[str]`: the next line of standard input without its `\n` or `\r\n`, `None` at the end; panics on text that is not UTF-8; needs `uses io` |
| `read_stdin()` | `bytes`: all that is left of standard input; needs `uses io` |
| `read_file(path)` | `Result[str, FileError]`: the whole file; needs `uses fs` |
| `write_file(path: p, text: t)` | `Result[i64, FileError]`: replaces the file's contents with `t`, creating it, `Ok` of the bytes written; needs `uses fs` |

Built-ins take positional arguments, except `write_file`: it has two parameters, so its
arguments are named like any call's. To print several values, interpolate:
`print("{a} and {b}")`.

### Text of values

- `i64`: decimal. `bool`: `true`/`false`. `str`: as is.
- **Floats**: the shortest digits that read back as the same value. Positional when the
  decimal exponent is in [-5, 17), otherwise scientific. Always contains a `.`: `3.0`, `0.1`,
  `1.0e17`, `1.5e-6`, `nan`, `inf`, `-inf`, `-0.0`.
- A float with a unit is followed by a space and its canonical unit: `9.81 m/s^2`. Aliases
  print expanded (`kg*m/s^2`).

## Input and files

```
fn main() uses io, fs {
    for name in args() {
        match read_file(name) {
            Ok(value: text) => { print("{name}: {text.len()} bytes"); }
            Err(error: _) => { print("{name}: cannot read it"); }
        }
    }
}
```

- A program reads its **arguments** with `args()` and its **standard input** a line at a time
  with `read_line()`. Both are io: a run of a program gives it both.
- `read_file` and `write_file` take a path relative to the **working directory** (or absolute);
  `/` separates directories on every platform. Text is UTF-8 both ways and written byte for byte.
- A failure is a `FileError`, a built-in enum whose variants are, in order, `NotFound` (no such
  file, or no such directory to write it in), `IsDirectory`, `Denied`, `NotText` (the file is
  not UTF-8), `Failed` (anything else) and `NotDirectory` (`list_dir` of a file). An empty path
  is `NotFound`.
- **Checked runs** (tests, goldens, `trace` and `profile`) stay deterministic and never change
  the disk: the working directory is the root the command runs in (the project's root), files
  are read from it, and writes are kept in memory for the run, where later reads of the same
  path (with `\` read as `/`) find them, and `list_dir` of the directory written lists them. A write still fails as the disk would make it fail: the
  directory it goes in must exist, and a directory cannot be overwritten. Tests have no
  arguments, an empty stdin and an empty environment. Executables keep their writes in memory too when
  `ANVIL_FS=memory` is set (any other value is an error).
- `anvil run file.anv -- a b` gives main the arguments `a` and `b`; stdin is the command's own,
  or a file with `--input FILE` (over MCP: `args` and `input`, and stdin is empty without it).
  `trace` and `profile` take the same, with stdin empty unless `--input` is given, and an empty
  environment. A golden's arguments, environment and stdin are in `golden/<file>.in` (see
  Projects); `anvil run` and executables see the process's environment.

## Programs, panics and exit codes

`main` takes no parameters and returns nothing or `i64`. Its `i64` result is the exit code and
must be in 0..=255; `exit(code)` ends the program earlier. A panic writes `panic: <message>` to
stderr and exits with code 101. Running out of stack is a panic too, `stack overflow: calls nested
too deeply`; the interpreter allows 10,000 nested calls, and native code as many as 64 MiB of
stack holds, so a program should not count on either depth.
Panic messages name the source location: `index 3 out of bounds for length 3 at
main.anv:12:9`.

Output is byte-identical on every platform (`\n` line endings) and every run of a program
without `uses clock` that is given the same arguments, input and files: random numbers are
seeded (see Effects) and nothing else comes from the environment.

## Tests

`test "name" { ... }` blocks run with `anvil test file.anv`. A test passes if it finishes and
fails if it panics (`assert`, a contract, overflow, a bad index). Tests may declare effects, after the name: `test "reads a file" uses fs { }`; each
starts with the random sequence at seed 0, the virtual clock at 0, no arguments, an empty stdin
and no files written (see Input and files).

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
anvil run     file.anv [--max-steps N] [--input FILE] [-- ARGS...]
                                       interpret (the reference semantics)
anvil build   file.anv [-o out] [--asm] [--release]
                                       native executable via Cranelift, for the host:
                                       Windows x64 (MSVC), Linux or macOS (x86-64, ARM64);
                                       --release goes through C (see below)
anvil test    file.anv [--native] [--cases N] [--seed N] [--max-steps N]
                                       run test blocks; `for all` tests on generated inputs
anvil fmt     [file.anv] [--check]     put the file (or every project file) in canonical
                                       layout; --check changes nothing
anvil fix     file.anv                 apply every mechanical fix until none is left
anvil fuzz    [--seed N] [--count N] [--release]
                                       random programs on both backends (see below)
anvil trace   file.anv [--test NAME] [--only F,G] [--limit N] [--input FILE] [-- ARGS...]
                                       a run as a timeline: calls, returns, prints
anvil profile file.anv [--test NAME] [--input FILE] [-- ARGS...]
                                       calls, steps and bytes per function and per line;
                                       where values were copied, and why they were shared
anvil cost file.anv [--only NAMES]     how each pure function's steps and bytes grow, and
                                       the steps and allocs bounds that fit
anvil outline file.anv                 items and signatures, without reading the whole file
anvil type-at file.anv LINE COL        type and declaration at a position
anvil refs    file.anv NAME            declaration and uses of NAME (fn, type, unit,
                                       Type.field, Enum.Variant, Enum.Variant.field), or
                                       of whatever is at LINE:COL (locals too)
anvil complete file.anv LINE COL       what can be written at a position
anvil lsp                              a language server on stdio for editors
anvil explain [CODE]                   long explanation of an error code or a lint
anvil spec                             print this document
anvil std     [MODULE]                 the standard library's modules and pub items, or
                                       one module's source
anvil docs    [--out DIR]              write a static site (default out/docs): this
                                       document, every error code and lint, the built-ins,
                                       the standard library
```

Every command takes `--json`. Every error has a stable code (`E0301`), an exact range and,
where one exists, fixes. A fix is `mechanical` (safe to apply without thought) or
`changes_meaning` (compiles, but may hide the real bug: understand the intent first);
`anvil fix` applies only mechanical ones (including the layout, last), and only in the file it
was given; errors in imported modules are reported with their own file. A syntax error stops
checking, but the parser goes on at the next item (`fn`, `struct`, `enum`, `unit`, `test`,
`allow`), so one run reports the first syntax error of every item. `type-at` and `refs` answer even for a file with
type errors, and follow imports: a declaration in a module is reported with its own file.
Where an item of the file has a syntax error, they leave that item out and answer from the rest
(items start at column 1 in canonical layout). `complete` takes the column the next character
would occupy: after `.` it lists the fields and methods of the receiver's type (map and array
methods with their element types filled in) or the variants of the enum named; elsewhere every
local and parameter in scope there, the file's items and the ones it imports, the built-ins, the
type names and the keywords, in that order. Only names starting with what is typed before the
column are listed. The item holding the column must parse up to it; a `.` or a name not yet
finished is fine. `--max-steps` stops a run after that many statements and loop iterations.

`anvil lsp` serves the Language Server Protocol on stdio. Each answer is the answer of a
command, on the editor's unsaved text: diagnostics are `check` (published to the file each is
in, imported modules included), hover and go-to-definition `type-at`, references `refs` at a
position, completion `complete` (after `.` too), formatting `fmt`, document symbols `outline`,
and code actions the fixes of the errors in range, mechanical ones preferred. Documents sync
whole; positions are UTF-32 when the editor offers it, otherwise UTF-16. Requests it does not
handle get an error, never silence.

`trace` and `profile` run `main`, or one test with `--test`, in the interpreter. A trace lists
each call with its arguments, each return value and each printed line, with the step and call
depth; values are written as Anvil source (`2.0[m]`, `Shape.Circle(1.5)`, `Some(3)`), cut
after 8 elements, 80 string bytes or 200 characters, saying how much was cut. `--only` keeps
the calls of the named functions (prints are always kept); `--limit` (default 1000) counts the
events left out. A profile gives, per function, its calls, its steps including callees and its
own steps, the bytes it allocated (by the model of `allocs`) and, of its own, those that copied
a value still shared when it changed (`copied`, `self_copied` in `--json`): the copies a move
would avoid (see Moves). It then lists where those copies were made, costliest first: each
site's line, function, bytes and times, and the calls that handed the shared values in, with
each argument read there from a variable or a place and why it was not moved (the statement
names it again, it is read again later, ...) or that it was (then something else holds the
value). When the call moved all it was handed, it also names the nearest call further up that did
not: that is what kept a second reference. A profile also lists the costliest lines, by the steps
and bytes of their own statements, callees not counted; `--json` has every line under `lines`. Both
count steps, never time, so they give the same answer everywhere; a panic or
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

Unknown keys, patterns that match nothing and files listed twice are errors. A file named
`testkit.anv` or `testkit_*.anv` holds helpers that tests share: what it makes `pub` needs no doc
comment and no approved API, though it is tested like any file. Project commands find `anvil.toml`
in the current directory or above:

```
anvil list                                  files: functions, tests, main, golden
anvil golden check [FILE] [--native] [--release]
                                            compare output with approved goldens
anvil golden approve FILE                   approve a program's current output
anvil api [FILE]                            public APIs against their approved ones
anvil api approve FILE                      approve a file's current public API
anvil usage [FILE]                          each run's steps, bytes and copies against the
                                            approved ones; any growth fails
anvil usage approve [FILE]                  approve a file's usage (or every changed one)
anvil measure [FILE]                        run programs natively: heap bytes, peak, objects
                                            and time, beside the interpreter's counts
anvil hot FILE [--release] [-- ARGS]        run a program natively and sample where its time
                                            goes, by function and by what the runtime does
anvil compare A B [-- ARGS]                 which of two programs runs faster, and whether
                                            that is more than the runs move on their own
anvil verify [--native]                     check, test and golden-check every file;
                                            store the findings in the record
anvil review [--native]                     verify, then write out/review.md for sign-off
anvil mutate FILE [--only NAMES]            change one thing at a time; report what no test
                                            or golden notices
anvil record query [KIND] [--open]          milestones, decisions, findings, runs
anvil record decide TITLE [BODY]            record a decision
anvil record status MILESTONE STATUS        open, active, done or dropped
anvil record import-roadmap [FILE]          milestones from docs/ROADMAP.md
anvil record export                         the record as Markdown
anvil watch [--native] [--runs N]           verify, and again after every change
```

A **golden** (`golden/<file>.out`) is a program's approved output: its exit code, its panic
message if any, the lines it wrote with eprint (`stderr: ...`), and every byte it printed. A
program that reads arguments, the environment or stdin is given them by `golden/<file>.in`
(the file's path from the project root, so `golden/src/main.anv.in` for `src/main.anv`; `verify` and
`golden approve` report a file there that belongs to no program, and where it should be),
written by hand: a line `arg: VALUE` per argument and `env: NAME=value` per variable, then
`---`, then the stdin byte for byte. Without one, it runs with none of them. `golden check --native` also builds each program and
requires the executable to match and to free every object. Only `golden approve` writes one,
after the output has been read. The **record** (`anvil_record.db`, not checked in) keeps
milestones, decisions, and every problem `verify` finds, with when it was first seen and when it
was resolved; a problem that comes back opens a new entry, so regressions keep their history.
When a program no longer matches its golden, `golden check` writes what it printed to
`out/golden/<file>.out` and a line diff to `out/golden/<file>.diff` (`.native.out` and
`.native.diff` for the executable).

A file's **public API** is approved the same way, in `golden/<file>.api`: one line per `pub`
item, sorted, with what other files must write to use it (a function's signature with its
parameter names, since calls name their arguments, and its contract; a struct's fields; an
enum's variants; an interface's methods; a unit). Docs are not part of it. `anvil api` prints
each file's API and how it differs from the approved one: an item **added**, **removed** or
**changed**; removed and changed items break the files that use them. `verify` reports every
difference, naming the project files whose programs include the file when the change breaks
them (they need not compile any more), and a file with pub items but no approved API. `anvil
api approve FILE` stores the current API after its changes have been read, and removes the
approved file when no pub items are left.

A file's **usage** is approved too, in `golden/<file>.usage`: one line per run, `main` under its
golden's input and each test (`test "name"`, every case of a property test together), with the
steps it takes, the bytes it allocates and the bytes it copies, as the interpreter counts them
(see Memory and Moves). The counts are the same on every machine, so a difference is a change to
the program or to the compiler. `anvil usage` lists each file's runs against the approved ones:
**more** of anything, a run not approved yet, or an approved run that no longer runs fails, and
`verify` reports each; **less** is listed but passes, to be approved so the gain is kept. A
failing test, or a main whose golden does not match, is not measured (it is a finding of its
own). `anvil usage approve FILE` stores the current numbers after the changes have been read;
without a file it does so for every file whose usage changed, skipping those that cannot be
measured.

`anvil hot FILE` shows where a compiled program's time really goes. It builds the program and runs
it natively (writes stay in memory), under its golden's input or the `--input` and arguments it is
given, looking at where it is about every millisecond. It reports the share of each of its functions, and
of what the runtime does for them: copying arrays and maps, making and freeing objects, maps, sorting,
text, printing, files and the system. Small functions are compiled into their callers and show
there. It lists the arrays that a loop checks for a second owner on every round, as the compiler
could not prove they have one. With `--release` it samples a release build, as a program ships;
more is compiled into its caller there. The numbers differ from run to run and nothing compares them; on
Windows only the main thread is sampled, so `parallel_map`'s workers show as waiting. `anvil profile`
is the exact count, in the interpreter, of steps, bytes and copies; use `hot` first to see whether
they are what costs.

`anvil compare A B` says which of two programs runs faster, in a way that holds on a machine with
other work and cores of different speeds, where separate timings can move by 2x. It builds both and
runs each once untimed, to compare what they print and how they exit, then times them in turn on one
logical processor of a fast core, at high priority on Windows, with the core's other one kept idle and
one worker (`ANVIL_THREADS=1`), `--rounds` times (7 unless asked, at least 3). The time is what the system
charged the process. A program is named as faster only when its best and its typical run both beat the
other's by more than these runs move on their own, which is reported: at least 2%, and more for short
runs, since Windows charges time in steps of about 16 ms, so give it an input that runs for a second or
more. A run past `--limit` seconds (600 unless asked) is stopped, as an error. Output or exit codes that
differ are a failure, with the times still shown, as the two are then not doing the same work.

`anvil measure` builds a program (or every project file with a main) and runs it natively under
its golden's input, with one `parallel_map` worker. It reports what the program's objects asked
of the heap as the runtime counts it: the bytes asked for (each time an array or text grows, all
of its new size), the most held at once, and the objects made, beside the interpreter's steps,
bytes and copied bytes. The model counts what a program makes, so the runtime's bytes are higher:
each object has a header (24 bytes for text, 48 for an array) and arrays double, and numbers
interpolated into text are temporaries. Heap numbers are the same on every run and every 64-bit
platform; the time is reported and never compared. A native run that differs from the
interpreter fails. `--native-only` leaves the interpreter out, for an input too big to wait for: no
steps, and nothing compared.

**Lints** are checks `verify` makes beyond type errors. Each counts what the project's tests
and golden-checked `main`s run, in the interpreter, across every project file (a module is
covered by the files that import it):

- **`untested`**: a function that none of them runs (`main` itself is left to its golden).
- **`unreached`**: a branch of a function that does run, which none of them takes: an `if`
  block whose condition is never true, an `else` block, a match arm, or a loop body that never
  runs. An empty block is no branch (it has nothing to test, like `None => {}` where no value
  is missing), and an `else if` is not a branch of its own; its blocks are. A function
  literal's branches belong to the function it is written in, and a generic's are counted over
  all its instances. `&&`, `||` and `?` are not branches.
- **`contract`**: a function with a contract that breaks it. Each pure function of the file
  that has a `requires`, an `ensures`, or a `steps` or `allocs` bound (a generic's instances too) is called on 100 generated
  arguments, as a property test's inputs are generated (small first, with edge values such as
  `i64::MAX`). For those its requires accept, it must return with its ensures holding; a
  panic (an ensures, a callee's requires, an index out of bounds, ...) is a finding, shrunk to
  small arguments and shown as Anvil values. A call that overflows i64 counts as one the
  requires excluded, since the language itself refuses it. A call that runs past 1,000,000
  steps is a finding (a loop that never ends, or inputs a requires should bound), and so is a
  requires that accepts none of the 100: the contract went unchecked. With a `steps` bound, the
  bound is computed from the arguments first and the call may take that many steps, no more:
  one more is a finding ("takes more than N steps"), shrunk like the others; arguments whose
  bound is more than a check may run are not run, since the function says they may take that
  long (`beyond` in `verify --json`). An `allocs` bound is computed the same way, and a call
  that allocates more bytes is a finding. A function with a bound is checked on wider arguments
  (arrays, maps and text up to 40 long) and also measured as `anvil cost` measures it: when
  its steps or bytes grow faster than the bound does (quadratic against linear, say), the bound holds
  for the sizes checked but will break for larger ones, and that is a finding too. A function with effects,
  or a function parameter, is not checked (`verify --json` says so in `contracts`).
- **`undocumented`**: a `pub` function, method, struct, enum, interface or unit with no doc
  comment.

Fix a finding with a test, for `contract` in the function or its contract, and for
`undocumented` with a doc; where that cannot be, write `allow untested "reason"` (or
`unreached`, `contract`, `undocumented`) on its own line before the `fn`. The reason is required, an unknown
lint or a repeated allow is an error, and an allow that switches nothing off is reported as
stale. `verify --json` gives each file's `untested` functions, `unreached` branches,
`branches` (`total` and `taken`) and `contracts` (each function's status: `held` with the
cases `checked`, `excluded`, `overflowed` and `beyond`, `broken`, `stopped` or `unchecked`); the review
sheet sums the branches.

**Mutation testing.** Coverage says what the tests run; `anvil mutate FILE` asks whether they
would notice it being wrong. Each **mutant** changes one thing in one function of FILE (a
function literal's belong to the function around it), in source order:

| Mutant | Change |
|---|---|
| operators | `+` and `-`, `*` to `/`, `/` to `*`, `%` to `/`, `==` and `!=`, `<` and `<=`, `>` and `>=`, `&&` and `\|\|`, `&` and `\|`, `<<` and `>>` |
| literals | an integer `n` to `n + 1`, `true` and `false` |
| `!`, `-`, `~` | left off |
| a call statement | left out |

Each mutant is put in canonical layout, then the tests and golden of FILE and of every project
file whose program includes it are run in the interpreter; each run may take ten times the
steps the unchanged one took (or `--max-steps`), so a mutant that loops is noticed too. The
unchanged files must pass first. A mutant is **killed** when a test fails, a golden differs,
a run runs out of steps or an importer no longer compiles (the JSON's `by` says which),
**invalid** when FILE no longer compiles, and otherwise it **survives**: a test is missing or
asserts too little. Any survivor fails the command. Only the runs that run the mutated statement run
again, as the others cannot notice; a survivor says whether any did (`reached` in the JSON), since
a statement nothing runs needs a test, and one that runs needs a test that looks.

Some survivors change nothing a program can see, like `<` for `<=` in a `min`, where both pick
the same value when the two are equal. `allow mutants "reason"` before the function, or before one
statement in it, excuses the survivors there (reported as `allowed`), and is stale, which also fails,
when none of those mutants survive. Mutation testing is not part of `verify`: it runs every test once per mutant.
The standard library's modules, which ship in the binary, are measured by their own tests.

`anvil-mcp` serves the same commands, except `watch` and `lsp`, as MCP tools over stdio (`type-at` is
`type_at`, `golden check` is `golden_check`), with the same arguments and the same JSON; a
failing check is a tool error. Over MCP, every run stops after 100,000,000 steps unless
`max_steps` says otherwise.

`anvil build --release` makes the executable a program ships as: slower to build (about a second,
against a fifth of one), faster to run (a third faster across the benchmark programs). It writes
the same lowered program the native backend compiles, before Cranelift optimizes it, as C, and
the C compiler the linker comes with already compiles it (MSVC's `cl /O2` on Windows, `cc -O2`
elsewhere), so nothing new needs installing. Every choice about what the program does is made
before either backend sees it, and the C pins down what C leaves open: integers wrap, shifts are
masked, floats are never fused into one rounding. A loop with no loop inside it, `for` or a `while`
counting up to a limit by a fixed step, also tests before it starts whether any array access whose
index is a straight line in its counter could fail. If none could, it runs without those checks,
which lets the C compiler vectorize it; if one could, it runs as written and stops at the same round
with the same message. `--asm` with `--release` gives the C, which says how many loops were cleared.
`golden check --release`, `fuzz --release` and `compare --release` run release builds too.

The interpreter and the native compiler must agree exactly: same stdout, same panic message,
same exit code, and the release backend with them. `anvil fuzz` checks it on random well-typed programs (seed N always makes the
same program); a program on which they differ is saved to `out/fuzz/<seed>.anv`. The one known difference: the interpreter panics past 10,000 nested calls,
while native code recurses until its 64 MiB stack runs out.
