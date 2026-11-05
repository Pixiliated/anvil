//! Typed IR produced by the checker. Names are resolved, call arguments are in parameter order,
//! and every operation that can fail carries its finished panic message, so the interpreter and
//! the native backend cannot disagree about what a failure says.

use crate::span::Span;
use crate::units::Unit;
use std::fmt;

pub type LocalId = usize;
pub type FuncId = usize;
/// An index into Program::branches.
pub type BranchId = usize;
pub type StructId = usize;
pub type EnumId = usize;

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Void,
    I64,
    F64(Unit),
    Bool,
    Str,
    /// Immutable bytes, any values 0..=255 (str is bytes that are UTF-8).
    Bytes,
    Struct(StructId),
    /// `[T]`: a growable array with value semantics (copied on write).
    Array(Box<Ty>),
    /// `Map[K, V]`: keys (i64 or str) in ascending order, each with one value; a value like arrays.
    Map(Box<Ty>, Box<Ty>),
    Enum(EnumId),
    /// A function value: a named function or a function literal with what it captured.
    Fn(Box<FnTy>),
    /// A type parameter (`T`) while a generic body is checked against its constraints. Only
    /// concrete instantiations reach the backends.
    Param(String),
    /// Produced after an error so one mistake does not cascade; never reaches a backend.
    Error,
}

/// The type of a function value: what it takes and gives, what it may do, and whether it
/// promises not to allocate.
#[derive(Clone, Debug, PartialEq)]
pub struct FnTy {
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub effects: Effects,
    pub noalloc: bool,
}

impl FnTy {
    /// Whether a value of this type can stand where `want` is expected: the same parameters and
    /// result, no effect `want` does not allow, and noalloc when `want` is.
    pub fn fits(&self, want: &FnTy) -> bool {
        self.params == want.params
            && self.ret == want.ret
            && self.effects.within(want.effects)
            && (self.noalloc || !want.noalloc)
    }
}

/// The effects a function or test may have, declared with `uses` (see SPEC, Effects).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Io,
    Fs,
    Clock,
    Random,
}

/// Every effect, in the order `uses` lists them.
pub const ALL_EFFECTS: [Effect; 4] = [Effect::Io, Effect::Fs, Effect::Clock, Effect::Random];

impl Effect {
    pub fn name(self) -> &'static str {
        match self {
            Effect::Io => "io",
            Effect::Fs => "fs",
            Effect::Clock => "clock",
            Effect::Random => "random",
        }
    }
    pub fn from_name(name: &str) -> Option<Effect> {
        ALL_EFFECTS.into_iter().find(|e| e.name() == name)
    }
    /// What performing it is called in messages: "performs io".
    pub fn doing(self) -> &'static str {
        match self {
            Effect::Io => "performs io",
            Effect::Fs => "reads or writes files",
            Effect::Clock => "reads the clock",
            Effect::Random => "draws random numbers",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Effects(u8);

impl Effects {
    pub fn has(self, e: Effect) -> bool {
        self.0 & (1 << e as u8) != 0
    }
    pub fn add(&mut self, e: Effect) {
        self.0 |= 1 << e as u8;
    }
    pub fn is_pure(self) -> bool {
        self.0 == 0
    }
    /// Whether every effect here is also in `other`.
    pub fn within(self, other: Effects) -> bool {
        self.0 & !other.0 == 0
    }
    pub fn iter(self) -> impl Iterator<Item = Effect> {
        ALL_EFFECTS.into_iter().filter(move |e| self.has(*e))
    }
}

/// A function written in C: its symbol, and the index into `Program::c_files` of the file that
/// defines it (None: the C runtime, such as libm's `cos`).
#[derive(Clone, Debug)]
pub struct ExternFn {
    pub symbol: String,
    pub file: Option<usize>,
}

/// A C file that extern functions name, with its text as read (so editors' unsaved text works).
#[derive(Clone, Debug)]
pub struct CFile {
    /// Its path, relative in the same way as the program's file names.
    pub name: String,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<(String, Ty)>,
}

/// A user enum, or one instantiation of the built-in `Option[T]` / `Result[T, E]`.
#[derive(Clone, Debug)]
pub struct EnumDef {
    /// As shown to users: `Shape`, `Option[i64]`.
    pub name: String,
    pub variants: Vec<VariantDef>,
}

#[derive(Clone, Debug)]
pub struct VariantDef {
    pub name: String,
    pub fields: Vec<(String, Ty)>,
}

/// Variant order of the built-in enums: the success variant is always 0, the failure 1.
pub const OPTION_SOME: usize = 0;
pub const OPTION_NONE: usize = 1;
pub const RESULT_OK: usize = 0;
pub const RESULT_ERR: usize = 1;

/// The variants of the built-in `FileError`, in order (see SPEC, Input and files).
pub const FILE_ERRORS: [&str; 6] = ["NotFound", "IsDirectory", "Denied", "NotText", "Failed", "NotDirectory"];
pub const FILE_NOT_FOUND: usize = 0;
pub const FILE_IS_DIRECTORY: usize = 1;
pub const FILE_DENIED: usize = 2;
pub const FILE_NOT_TEXT: usize = 3;
pub const FILE_FAILED: usize = 4;
pub const FILE_NOT_DIRECTORY: usize = 5;

#[derive(Clone, Debug)]
pub struct Local {
    pub name: String,
    pub ty: Ty,
}

/// A runtime check (requires, ensures) with its complete panic message.
#[derive(Clone, Debug)]
pub struct Check {
    pub expr: Expr,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct Func {
    pub name: String,
    /// The name in the declaration.
    pub span: Span,
    /// `allow LINT "reason"` lines, as (lint, reason).
    pub allows: Vec<(String, String)>,
    /// Index into `Program::modules` of the file that declares it (`span` is in that file).
    pub module: usize,
    /// An instance of a generic function (named `max[i64]`); `span` is the generic's.
    pub instance: bool,
    pub params: Vec<LocalId>,
    pub ret: Ty,
    pub effects: Effects,
    /// Implemented in C: the body is empty, and calls go to the C function.
    pub external: Option<ExternFn>,
    pub requires: Vec<Check>,
    pub ensures: Vec<Check>,
    /// `steps EXPR`: how many steps a call may take (an i64 of the parameters), with the text
    /// the contract lint reports; checked by verify on generated arguments, never at run time.
    pub steps: Option<Check>,
    /// `allocs EXPR`: how many bytes a call may allocate (by the model `profile` and `cost` count
    /// with), checked the same way as `steps`.
    pub allocs: Option<Check>,
    /// Bound to the return value while ensures clauses run.
    pub result_local: Option<LocalId>,
    pub locals: Vec<Local>,
    pub body: Vec<Stmt>,
    /// A function literal: its first n params are the values it captured, given when the
    /// closure is made; the rest are its own. None for a declared function.
    pub captures: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct Test {
    pub name: String,
    /// The generated inputs of a property test (`for all`), as locals; empty for a plain test.
    pub inputs: Vec<LocalId>,
    pub effects: Effects,
    pub locals: Vec<Local>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct Program {
    pub structs: Vec<StructDef>,
    pub enums: Vec<EnumDef>,
    pub funcs: Vec<Func>,
    pub tests: Vec<Test>,
    pub main: Option<FuncId>,
    /// The program's files: the root first, then each imported module. Functions from a module
    /// are named `module.name`.
    pub modules: Vec<String>,
    /// Every generic function, instantiated or not, so coverage can see the ones never called.
    pub generic_fns: Vec<GenericFn>,
    /// Every branch of every function, for the `unreached` lint; Stmt::Reached counts a run.
    pub branches: Vec<Branch>,
    /// The C files extern functions are defined in.
    pub c_files: Vec<CFile>,
}

/// A block of a function that runs only sometimes: an `if` or `else` block, a match arm or a
/// loop body. An instance shares its generic's branches.
#[derive(Clone, Debug)]
pub struct Branch {
    /// The module it is written in, and its block (which starts at its `{`).
    pub module: usize,
    pub at: Span,
    /// The name of the declared function it is in; a function literal's branches belong to the
    /// function it is written in.
    pub owner: Span,
    pub kind: BranchKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BranchKind {
    Then,
    Else,
    /// The arm of this variant.
    Arm(String),
    Loop,
}

impl BranchKind {
    /// What the lint calls it.
    pub fn describe(&self) -> String {
        match self {
            BranchKind::Then => "the `if` block (its condition is never true)".into(),
            BranchKind::Else => "the `else` block".into(),
            BranchKind::Arm(v) => format!("the `{v}` arm"),
            BranchKind::Loop => "the loop body".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct GenericFn {
    pub name: String,
    pub module: usize,
    pub span: Span,
    pub allows: Vec<(String, String)>,
}

/// One step from a local to the place being assigned or mutated.
#[derive(Clone, Debug)]
pub enum Step {
    Field(usize),
    /// Bounds-checked; `site` is the source location named by the panic.
    Index {
        index: Expr,
        site: String,
    },
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub variant: usize,
    /// (field index in the variant, local bound to it)
    pub bindings: Vec<(usize, LocalId)>,
    pub body: Vec<Stmt>,
}

/// Evaluation order, shared by both backends: the right-hand value first, then the index
/// expressions of the place from left to right, then bounds checks as the place is reached.
#[derive(Clone, Debug)]
pub enum Stmt {
    Let(LocalId, Expr),
    /// Assign to a local or a place inside it.
    Assign(LocalId, Vec<Step>, Expr),
    Expr(Expr),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    While(Expr, Vec<Stmt>),
    /// `for i in start..end`, end exclusive, both evaluated once.
    For(LocalId, Expr, Expr, Vec<Stmt>),
    /// `for x in array`: iterates a snapshot held in the hidden local `held`.
    ForEach {
        var: LocalId,
        held: LocalId,
        iter: Expr,
        body: Vec<Stmt>,
    },
    /// The scrutinee is stored in the hidden local `held`; exactly one arm per variant.
    Match {
        held: LocalId,
        scrutinee: Expr,
        arms: Vec<Arm>,
    },
    Return(Option<Expr>),
    Break,
    Continue,
    /// Counts a run of a branch for coverage, at the start of its block; does nothing else, and
    /// is not a step.
    Reached(BranchId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// Bit operations on i64. `Shr` keeps the sign; `UShr` (`unsigned_shr`) fills with zeros.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitOp {
    And,
    Or,
    Xor,
    Shl,
    Shr,
    UShr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug)]
pub enum Builtin {
    /// Prints each argument (unit-carrying floats with their unit) then a newline.
    Print,
    Assert {
        message: String,
    },
    Sqrt,
    AbsI64 {
        message: String,
    },
    AbsF64,
    ToF64,
    ToI64 {
        message: String,
    },
    /// Length of a str (bytes), an array (elements) or a map (keys).
    Len,
    /// `m.get(k)`: Option of the value; args are the map and the key.
    MapGet,
    MapHas,
    /// All keys in ascending order, or the values in the same order.
    MapKeys,
    MapValues,
    /// str methods; args are the receiver and the other str. Find gives an Option of the byte offset.
    StrContains,
    StrFind,
    StrStartsWith,
    StrEndsWith,
    /// Splits at every occurrence of a non-empty separator; panics with `message` if it is empty.
    StrSplit {
        message: String,
    },
    /// Nanoseconds on the run's clock (see `clock_ns` in SPEC): real, or virtual in tests.
    ClockNs,
    /// A uniformly random i64 in 0..n; unless n > 0, panics naming `site`.
    RandomBelow {
        site: String,
    },
    /// A uniformly random f64 in [0, 1), a multiple of 2^-53.
    RandomF64,
    /// Restarts the random sequence from a seed.
    RandomSeed,
    /// The program's command-line arguments as `[str]`.
    Args,
    /// The next line of stdin without its line ending: Option[str]; panics with `message` on
    /// text that is not UTF-8.
    ReadLine {
        message: String,
    },
    /// The whole file as text: Result[str, FileError]. Args: the path.
    ReadFile,
    /// Replaces a file's contents: Result[i64, FileError], Ok of the bytes written. Args: the
    /// path and the text.
    WriteFile,
    /// The str without leading and trailing spaces, tabs, `\r` and `\n` (`trim_text`).
    StrTrim,
    /// `parse_i64_text` and `parse_f64_text` as an Option.
    StrParseI64,
    StrParseF64,
    /// i64 arithmetic that wraps around instead of panicking; args: the receiver and the other.
    WrappingAdd,
    WrappingSub,
    WrappingMul,
    /// `bytes_of([i64])`; panics naming `site` on a value outside 0..=255 (`byte_message`).
    BytesOf {
        site: String,
    },
    /// The whole file: Result[bytes, FileError]. Args: the path.
    ReadBytes,
    /// Like WriteFile, with bytes. Args: the path and the data.
    WriteBytes,
    /// `s.to_bytes()`: the str's UTF-8 bytes.
    StrToBytes,
    /// `b.to_str()`: Some when the bytes are UTF-8.
    BytesToStr,
    /// `b.hex()`: two lowercase hex digits per byte (`hex_text`).
    BytesHex,
    /// `b.to_array()`: each byte as an i64.
    BytesToArray,
    /// `s.replace(old: a, new: b)`: every `a`, left to right, as `b`; panics with `message` when
    /// `a` is empty.
    StrReplace {
        message: String,
    },
    /// ASCII letters only (`case_text`); other characters stay.
    StrUpper,
    StrLower,
    /// `s.chars()`: each character as a str of its own.
    StrChars,
    /// `xs.join(sep)` on [str].
    ArrJoin,
    /// `x.fixed(n)`: `n` digits after the point (`fixed_text`); panics with `message` unless n is
    /// in 0..=17.
    FloatFixed {
        message: String,
    },
    /// `xs.sorted()` on [i64], [f64] or [str]: ascending and stable; floats panic with `message`
    /// on NaN.
    ArrSorted {
        message: String,
    },
    /// `xs.reversed()`
    ArrReversed,
    /// Like Print, to stderr.
    EPrint,
    /// Ends the program with the code; panics naming `site` outside 0..=255, or in a test.
    Exit {
        site: String,
    },
    /// The environment variable's value: Option[str]. `ANVIL_` names are the runtime's: None.
    Env,
    /// The names in a directory, sorted: Result[[str], FileError].
    ListDir,
    /// `xs.parallel_map(f)`: the same as calling the pure `f` on each element in order, computed
    /// on worker threads; args: the array and the function value.
    ParallelMap,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Ty,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Local(LocalId),
    Field(Box<Expr>, usize),
    Call(FuncId, Vec<Expr>),
    /// A declared function as a value.
    FnRef(FuncId),
    /// A function literal as a value: `func` with the captured values, in the order of its
    /// first params.
    Closure {
        func: FuncId,
        captures: Vec<Expr>,
    },
    /// Calls a function value with arguments in parameter order (the value is evaluated first).
    CallValue(Box<Expr>, Vec<Expr>),
    StructLit(StructId, Vec<Expr>),
    ArrayLit(Vec<Expr>),
    /// Construct `variant` of an enum; `fields` are that variant's fields in order.
    Variant {
        enum_id: EnumId,
        variant: usize,
        fields: Vec<Expr>,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
        site: String,
    },
    /// `xs[from..to]` of an array or a str (byte offsets on character boundaries): a new value.
    Slice {
        base: Box<Expr>,
        from: Box<Expr>,
        to: Box<Expr>,
        site: String,
    },
    /// Entries evaluated in order, key then value; a repeated key panics with `duplicate`.
    MapLit {
        entries: Vec<(Expr, Expr)>,
        duplicate: String,
    },
    /// `m.set(key: k, value: v)` on the map at a place: replaces or inserts.
    MapSet {
        local: LocalId,
        path: Vec<Step>,
        key: Box<Expr>,
        value: Box<Expr>,
    },
    /// `m.remove(k)` on the map at a place; panics with `missing` if the key is absent.
    MapRemove {
        local: LocalId,
        path: Vec<Step>,
        key: Box<Expr>,
        missing: String,
    },
    /// Concatenation of the text of each part (str, i64, f64 with its unit, bool).
    Interp(Vec<Expr>),
    StrConcat(Box<Expr>, Box<Expr>),
    /// `xs.push(v)` on the array at a place.
    Push {
        local: LocalId,
        path: Vec<Step>,
        value: Box<Expr>,
    },
    /// `xs.pop()`: removes and returns the last element; panics with `message` when empty.
    Pop {
        local: LocalId,
        path: Vec<Step>,
        message: String,
    },
    /// `e?` on Option/Result: the success payload, or return the failure from the function.
    /// The value is first stored in the hidden local `held`; `ret_enum` is the function's return type.
    Try {
        held: LocalId,
        inner: Box<Expr>,
        ret_enum: EnumId,
    },
    /// Checked integer arithmetic. `overflow` and `div_zero` are the panic messages.
    IArith {
        op: IOp,
        l: Box<Expr>,
        r: Box<Expr>,
        overflow: String,
        div_zero: String,
    },
    FArith(FOp, Box<Expr>, Box<Expr>),
    Cmp(CmpOp, Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    INeg {
        e: Box<Expr>,
        overflow: String,
    },
    FNeg(Box<Expr>),
    /// A bit operation on two i64s; a shift panics naming `site` unless its amount is in 0..=63
    /// (`shift_message`). Bits shifted out are dropped.
    Bits {
        op: BitOp,
        l: Box<Expr>,
        r: Box<Expr>,
        site: String,
    },
    /// `~x`
    BitNot(Box<Expr>),
    Builtin(Builtin, Vec<Expr>),
}

/// Whether evaluating `e` reads or writes local `l` anywhere (function literals' bodies are
/// functions of their own; what they captured is read where the literal is).
pub fn uses_local(e: &Expr, l: LocalId) -> bool {
    let any = |es: &[Expr]| es.iter().any(|x| uses_local(x, l));
    let path = |p: &[Step]| p.iter().any(|s| matches!(s, Step::Index { index, .. } if uses_local(index, l)));
    match &e.kind {
        ExprKind::Local(x) => *x == l,
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_) | ExprKind::FnRef(_) => false,
        ExprKind::Push { local, path: p, value } => *local == l || path(p) || uses_local(value, l),
        ExprKind::Pop { local, path: p, .. } => *local == l || path(p),
        ExprKind::MapSet { local, path: p, key, value } => {
            *local == l || path(p) || uses_local(key, l) || uses_local(value, l)
        }
        ExprKind::MapRemove { local, path: p, key, .. } => *local == l || path(p) || uses_local(key, l),
        ExprKind::Slice { base, from, to, .. } => uses_local(base, l) || uses_local(from, l) || uses_local(to, l),
        ExprKind::MapLit { entries, .. } => entries.iter().any(|(k, v)| uses_local(k, l) || uses_local(v, l)),
        ExprKind::CallValue(callee, es) => uses_local(callee, l) || any(es),
        ExprKind::Bits { l: a, r: b, .. }
        | ExprKind::IArith { l: a, r: b, .. }
        | ExprKind::FArith(_, a, b)
        | ExprKind::Cmp(_, a, b)
        | ExprKind::And(a, b)
        | ExprKind::Or(a, b)
        | ExprKind::StrConcat(a, b)
        | ExprKind::Index { base: a, index: b, .. } => uses_local(a, l) || uses_local(b, l),
        ExprKind::BitNot(x) | ExprKind::Field(x, _) | ExprKind::Not(x) | ExprKind::FNeg(x) | ExprKind::INeg { e: x, .. } => {
            uses_local(x, l)
        }
        ExprKind::Try { held, inner, .. } => *held == l || uses_local(inner, l),
        ExprKind::Closure { captures: es, .. }
        | ExprKind::Call(_, es)
        | ExprKind::StructLit(_, es)
        | ExprKind::ArrayLit(es)
        | ExprKind::Interp(es)
        | ExprKind::Builtin(_, es)
        | ExprKind::Variant { fields: es, .. } => any(es),
    }
}

/// `l = l + a + b + ...` on a str or bytes local, where no part reads `l`: the parts to append,
/// in order. Both backends append them in place when `l` holds the only reference to its text,
/// which grows into room that doubles, so building text this way is linear, not quadratic.
pub fn append_parts(l: LocalId, e: &Expr) -> Option<Vec<&Expr>> {
    let mut parts = Vec::new();
    let mut cur = e;
    loop {
        match &cur.kind {
            ExprKind::StrConcat(left, right) => {
                parts.push(&**right);
                cur = left;
            }
            ExprKind::Local(x) if *x == l && !parts.is_empty() => break,
            _ => return None,
        }
    }
    parts.reverse();
    if parts.iter().any(|p| uses_local(p, l)) {
        return None;
    }
    Some(parts)
}

/// The room text of `len` bytes has to grow into when it is appended to in place: a power of two
/// above it, at least 16. Mirrors `text_room` in the C runtime.
pub fn text_room(len: u64) -> u64 {
    (len + 1).next_power_of_two().max(16)
}

// ---- shared runtime semantics: both backends must use these ----

pub const PANIC_EXIT_CODE: i32 = 101;

pub fn overflow_message(op: &str, site: &str) -> String {
    format!("integer overflow in `{op}` at {site}")
}
/// Whether a panic is integer overflow (of `+`, `-`, `*`, `/`, negation or `abs`).
pub fn is_overflow(message: &str) -> bool {
    message.starts_with("integer overflow in `")
}
pub fn div_zero_message(site: &str) -> String {
    format!("division by zero at {site}")
}
pub fn main_exit_message(code: i64) -> String {
    format!("main returned {code}, outside the exit code range 0..=255")
}
/// Mirrors anvil_panic_index in the C runtime.
pub fn index_message(index: i64, len: i64, site: &str) -> String {
    format!("index {index} out of bounds for length {len} at {site}")
}
pub fn pop_message(site: &str) -> String {
    format!("pop from an empty array at {site}")
}
/// Mirrors the bounds check of anvil_arr_slice and anvil_str_slice in the C runtime.
pub fn slice_message(from: i64, to: i64, len: i64, site: &str) -> String {
    format!("slice {from}..{to} out of bounds for length {len} at {site}")
}
/// Mirrors the character check of anvil_str_slice.
pub fn slice_char_message(from: i64, to: i64, site: &str) -> String {
    format!("slice {from}..{to} splits a character at {site}")
}
pub fn split_message(site: &str) -> String {
    format!("split with an empty separator at {site}")
}
pub fn duplicate_key_message(site: &str) -> String {
    format!("duplicate key in the map literal at {site}")
}
pub fn exit_message(code: i64, site: &str) -> String {
    format!("exit({code}) is outside the exit code range 0..=255 at {site}")
}
pub fn exit_in_test_message(code: i64, site: &str) -> String {
    format!("exit({code}) in a test at {site}; a test ends by returning")
}

/// Names `env` never reads: the runtime's own settings.
pub const RESERVED_ENV_PREFIX: &str = "ANVIL_";

pub fn replace_message(site: &str) -> String {
    format!("replace with an empty `old` at {site}")
}
pub fn fixed_message(site: &str) -> String {
    format!("fixed takes 0..=17 digits at {site}")
}
pub fn sorted_message(site: &str) -> String {
    format!("sorted found nan, which has no order, at {site}")
}

/// `to_upper` and `to_lower`: ASCII letters change, every other character stays. Mirrors
/// anvil_str_case.
pub fn case_text(s: &str, upper: bool) -> String {
    if upper {
        s.to_ascii_uppercase()
    } else {
        s.to_ascii_lowercase()
    }
}

/// `x.fixed(n)`: the value rounded to n digits after the point, the nearest such decimal (ties
/// to even, as the exact binary value decides); `nan`, `inf` and `-inf` as in print. Mirrors
/// anvil_f64_fixed.
pub fn fixed_text(v: f64, n: usize) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    format!("{v:.n$}")
}

/// Mirrors anvil_check_shift in the C runtime.
pub fn shift_message(n: i64, site: &str) -> String {
    format!("shift by {n} at {site}; shifts take 0..=63")
}

/// Mirrors anvil_bytes_of in the C runtime.
pub fn byte_message(v: i64, site: &str) -> String {
    format!("bytes_of got {v}, which is not a byte (0..=255), at {site}")
}

/// Mirrors anvil_bytes_hex.
pub fn hex_text(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Mirrors anvil_read_line in the C runtime.
pub fn read_line_message(site: &str) -> String {
    format!("read_line read text that is not UTF-8 at {site}")
}

/// What `trim` removes from both ends: space, tab, `\r` and `\n`. Mirrors anvil_str_trim.
pub fn trim_text(s: &str) -> &str {
    s.trim_matches(|c| matches!(c, ' ' | '\t' | '\r' | '\n'))
}

/// `parse_i64`: an optional `-` and one or more ASCII digits, nothing else, in range. Mirrors
/// anvil_str_parse_i64.
pub fn parse_i64_text(s: &str) -> Option<i64> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `parse_f64`: an optional `-`, digits, optionally `.` and digits, optionally `e` or `E`, an
/// optional sign and digits; the nearest double, which must be finite. Mirrors
/// anvil_str_parse_f64 (strtod, also correctly rounded).
pub fn parse_f64_text(s: &str) -> Option<f64> {
    if !float_syntax(s.as_bytes()) {
        return None;
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn float_syntax(b: &[u8]) -> bool {
    let mut i = usize::from(b.first() == Some(&b'-'));
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i > start
    };
    if !digits(&mut i) {
        return false;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        if !digits(&mut i) {
            return false;
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        if !digits(&mut i) {
            return false;
        }
    }
    i == b.len()
}

/// The key under which a run that keeps writes in memory stores a file: the path as written,
/// with `\` read as `/`. Mirrors fs_key in the C runtime.
pub fn memory_file_key(path: &str) -> String {
    path.replace('\\', "/")
}

/// Mirrors anvil_random_below in the C runtime.
pub fn random_below_message(n: i64, site: &str) -> String {
    format!("random_below({n}) needs a bound above 0 at {site}")
}

/// The one random sequence of every run (SplitMix64; mirrored by the C runtime). It starts from
/// seed 0, so a program that never calls `random_seed` draws the same numbers every time.
#[derive(Clone, Debug, Default)]
pub struct Random(u64);

impl Random {
    pub fn seeded(seed: i64) -> Random {
        Random(seed as u64)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in 0..n for n > 0: draws below 2^64 mod n are rejected, so no value is favoured.
    pub fn below(&mut self, n: i64) -> i64 {
        let n = n as u64;
        let reject = n.wrapping_neg() % n;
        loop {
            let x = self.next_u64();
            if x >= reject {
                return (x % n) as i64;
            }
        }
    }
    pub fn unit_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
    }
}

/// The virtual clock of tests and goldens: the n-th read (from 0) gives n milliseconds.
pub const VIRTUAL_TICK_NS: i64 = 1_000_000;

pub fn missing_key_message(site: &str) -> String {
    format!("remove of a key the map does not have at {site}")
}

/// i64 -> f64 range limits for to_i64: values must lie in [-2^63, 2^63).
pub const TO_I64_MIN: f64 = -9223372036854775808.0;
pub const TO_I64_MAX_EXCLUSIVE: f64 = 9223372036854775808.0;

/// Anvil's float text: shortest round-trip digits; positional when the decimal exponent is in
/// [-5, 17), scientific otherwise; always contains a `.`. Mirrors anvil_format_f64 in the C runtime.
pub fn format_f64(v: f64) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v < 0.0 { "-inf".into() } else { "inf".into() };
    }
    let mut sci = String::new();
    for prec in 1..=17usize {
        sci = format!("{:.*e}", prec - 1, v);
        if sci.parse::<f64>().unwrap() == v {
            break;
        }
    }
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp10: i32 = exp.parse().unwrap();
    let neg = mant.starts_with('-');
    let mut digits: Vec<u8> = mant.bytes().filter(|b| b.is_ascii_digit()).collect();
    while digits.len() > 1 && *digits.last().unwrap() == b'0' {
        digits.pop();
    }
    let nd = digits.len() as i32;
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if (-5..17).contains(&exp10) {
        if exp10 < 0 {
            out.push_str("0.");
            for _ in 0..(-exp10 - 1) {
                out.push('0');
            }
            out.push_str(std::str::from_utf8(&digits).unwrap());
        } else {
            let mut i = 0;
            while i <= exp10 {
                out.push(if i < nd { digits[i as usize] as char } else { '0' });
                i += 1;
            }
            out.push('.');
            if i >= nd {
                out.push('0');
            }
            while i < nd {
                out.push(digits[i as usize] as char);
                i += 1;
            }
        }
    } else {
        out.push(digits[0] as char);
        out.push('.');
        if nd == 1 {
            out.push('0');
        }
        out.push_str(std::str::from_utf8(&digits[1..]).unwrap());
        out.push_str(&format!("e{exp10}"));
    }
    out
}

pub struct TyDisplay<'a> {
    pub ty: &'a Ty,
    pub structs: &'a [StructDef],
    pub enums: &'a [EnumDef],
}

impl fmt::Display for TyDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.ty {
            Ty::Void => write!(f, "nothing"),
            Ty::I64 => write!(f, "i64"),
            Ty::F64(u) if u.is_dimensionless() => write!(f, "f64"),
            Ty::F64(u) => write!(f, "f64[{u}]"),
            Ty::Bool => write!(f, "bool"),
            Ty::Str => write!(f, "str"),
            Ty::Bytes => write!(f, "bytes"),
            Ty::Struct(id) => write!(f, "{}", self.structs[*id].name),
            Ty::Array(t) => write!(f, "[{}]", TyDisplay { ty: t, structs: self.structs, enums: self.enums }),
            Ty::Map(k, v) => write!(
                f,
                "Map[{}, {}]",
                TyDisplay { ty: k, structs: self.structs, enums: self.enums },
                TyDisplay { ty: v, structs: self.structs, enums: self.enums }
            ),
            Ty::Enum(id) => write!(f, "{}", self.enums[*id].name),
            Ty::Fn(t) => {
                let shown = |ty| TyDisplay { ty, structs: self.structs, enums: self.enums }.to_string();
                let params: Vec<String> = t.params.iter().map(shown).collect();
                write!(f, "fn({})", params.join(", "))?;
                match &t.ret {
                    Ty::Void => {}
                    Ty::Fn(_) => write!(f, " -> ({})", shown(&t.ret))?,
                    r => write!(f, " -> {}", shown(r))?,
                }
                let effects: Vec<&str> = t.effects.iter().map(Effect::name).collect();
                if !effects.is_empty() {
                    write!(f, " uses {}", effects.join(", "))?;
                }
                if t.noalloc {
                    write!(f, " noalloc")?;
                }
                Ok(())
            }
            Ty::Param(name) => write!(f, "{name}"),
            Ty::Error => write!(f, "<error>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::format_f64;

    #[test]
    fn float_text() {
        let cases = [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (3.5, "3.5"),
            (0.1, "0.1"),
            (100.0, "100.0"),
            (1e16, "10000000000000000.0"),
            (1e17, "1.0e17"),
            (1.5e-5, "0.000015"),
            (1.5e-6, "1.5e-6"),
            (123456.789, "123456.789"),
            (f64::MAX, "1.7976931348623157e308"),
            (5e-324, "5.0e-324"),
            (2.0f64.sqrt(), "1.4142135623730951"),
        ];
        for (v, s) in cases {
            assert_eq!(format_f64(v), s, "formatting {v:e}");
        }
    }
}
