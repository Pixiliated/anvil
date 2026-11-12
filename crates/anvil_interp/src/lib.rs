//! Reference interpreter over the typed IR. It defines what a program means; the native backend
//! is tested against it (same stdout, same panic message, same exit code).

pub mod gen;
mod show;
pub use show::{show, SHOWN_BYTES, SHOWN_CHARS, SHOWN_ITEMS};

use anvil_front::ir::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::io::{BufRead, Write};
use std::path::Path;
use std::rc::{Rc, Weak};

/// Arrays and maps share storage until written (Rc::make_mut), which gives value semantics
/// cheaply. The derived `==` is Anvil's: structural, with floats compared as IEEE values.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Void,
    I64(i64),
    F64(f64),
    Bool(bool),
    Str(Rc<str>),
    Bytes(Rc<[u8]>),
    Struct(Fields),
    Array(Rc<Vec<Value>>),
    /// Variant index and that variant's fields.
    Enum(usize, Fields),
    /// Entries in ascending key order (i64 keys by value, str keys bytewise).
    Map(Rc<Vec<(Value, Value)>>),
    /// A function value (never compared: the checker refuses `==` on functions).
    Fn(Rc<Closure>),
}

/// The fields of a struct or a variant, shared until written like an array, so reading a value
/// that holds others copies none of them. A value with no fields allocates nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fields(Option<Rc<[Value]>>);

impl Fields {
    /// The fields, ready to be changed: unshared first if anything else holds them.
    pub fn make_mut(&mut self) -> &mut [Value] {
        let Some(fields) = &mut self.0 else { return &mut [] };
        if Rc::get_mut(fields).is_none() {
            *fields = fields.iter().cloned().collect();
        }
        Rc::get_mut(fields).expect("just unshared")
    }
}

impl std::ops::Deref for Fields {
    type Target = [Value];
    fn deref(&self) -> &[Value] {
        self.0.as_deref().unwrap_or(&[])
    }
}

impl From<Vec<Value>> for Fields {
    fn from(fields: Vec<Value>) -> Fields {
        Fields((!fields.is_empty()).then(|| fields.into()))
    }
}

impl FromIterator<Value> for Fields {
    fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Fields {
        iter.into_iter().collect::<Vec<Value>>().into()
    }
}

/// A function and the values it captured, which are passed before the call's arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct Closure {
    pub func: FuncId,
    pub captures: Vec<Value>,
}

/// The order of map keys: i64 by value, str bytewise (as `<` on str).
fn key_order(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::I64(x), Value::I64(y)) => x.cmp(y),
        (Value::Str(x), Value::Str(y)) => x.as_bytes().cmp(y.as_bytes()),
        _ => unreachable!("map key {a:?}"),
    }
}

/// Ok(index) of `key`, or Err(the index where it would go).
pub(crate) fn find_key(entries: &[(Value, Value)], key: &Value) -> Result<usize, usize> {
    entries.binary_search_by(|(k, _)| key_order(k, key))
}

/// Why evaluation stopped early: a panic (the text after "panic: "), `?` returning a failure,
/// or the step limit running out.
#[derive(Debug, PartialEq)]
enum Unwind {
    Panic(String),
    Return(Value),
    OutOfSteps,
    /// `exit(code)`
    Exit(i32),
}

/// Boxed, so a result fits in registers: most evaluation never fails.
type R<T> = Result<T, Box<Unwind>>;

fn panic<T>(message: impl Into<String>) -> R<T> {
    Err(Box::new(Unwind::Panic(message.into())))
}

/// Interpreted calls deeper than this panic instead of exhausting the host stack.
pub const MAX_CALL_DEPTH: usize = 10_000;

/// How a statement ended. A return's value waits in `Interp::returned`, so this fits in a register.
#[derive(Clone, Copy)]
enum Flow {
    Normal,
    Break,
    Continue,
    Return,
}

/// Which clock `clock_ns` reads: the host's monotonic clock (`anvil run`), or the virtual clock
/// of tests, goldens, traces and profiles, whose n-th read (from 0) gives n milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clock {
    Real,
    Virtual,
}

/// Calls the C functions of extern declarations (anvil_codegen builds them into a library).
pub trait Foreign {
    /// Calls extern function `func` with its i64 and f64 arguments as bits; the result's bits
    /// (0 for a function that returns nothing).
    fn call(&mut self, func: FuncId, args: &[u64]) -> u64;
}

/// Where `read_file` and `write_file` find files, and whether writes reach the disk.
#[derive(Clone, Copy, Debug)]
pub struct Files<'h> {
    /// Relative paths are relative to this directory (the run's working directory).
    pub root: &'h Path,
    /// Writes are kept in memory for the run and read back from there, so a checked run (a
    /// test, a golden, a trace or a profile) never changes the disk. `anvil run` writes.
    pub memory: bool,
}

/// Where `env` reads: this process's environment (`anvil run`), or the variables a checked run
/// is given (a golden's `.in`; none for tests).
#[derive(Clone, Copy, Debug)]
pub enum Env<'h> {
    Real,
    Given(&'h [(String, String)]),
}

/// What a run takes from outside the program.
pub struct Host<'h> {
    pub clock: Clock,
    /// The program's C functions; None for a program without externs.
    pub foreign: Option<&'h mut dyn Foreign>,
    /// What `args()` returns.
    pub args: &'h [String],
    /// What `read_line` reads; None is an empty input.
    pub stdin: Option<&'h mut dyn BufRead>,
    pub files: Files<'h>,
    /// Where `eprint` writes.
    pub stderr: &'h mut dyn Write,
    pub env: Env<'h>,
}

impl Host<'_> {
    pub fn reborrow(&mut self) -> Host<'_> {
        Host {
            clock: self.clock,
            foreign: self.foreign.as_deref_mut().map(|f| f as &mut dyn Foreign),
            args: self.args,
            stdin: self.stdin.as_deref_mut().map(|s| s as &mut dyn BufRead),
            files: self.files,
            stderr: &mut *self.stderr,
            env: self.env,
        }
    }
}

/// Hashes addresses, which are spread out already.
#[derive(Default)]
struct AddressHash(u64);

impl Hasher for AddressHash {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, _: &[u8]) {
        unreachable!("only addresses are hashed");
    }
    fn write_usize(&mut self, n: usize) {
        self.0 = (n as u64 >> 3).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

type ByAddress<V> = HashMap<usize, V, BuildHasherDefault<AddressHash>>;

/// The indices of a place, usually few enough to need no allocation.
struct Indices {
    inline: [i64; 6],
    n: usize,
    more: Vec<i64>,
}

impl Indices {
    fn new() -> Indices {
        Indices { inline: [0; 6], n: 0, more: Vec::new() }
    }

    fn push(&mut self, i: i64) {
        if self.n < self.inline.len() {
            self.inline[self.n] = i;
        } else {
            if self.more.is_empty() {
                self.more.extend_from_slice(&self.inline);
            }
            self.more.push(i);
        }
        self.n += 1;
    }
}

impl std::ops::Deref for Indices {
    type Target = [i64];
    fn deref(&self) -> &[i64] {
        if self.more.is_empty() {
            &self.inline[..self.n]
        } else {
            &self.more
        }
    }
}

/// What a failed `assert` compared, written as Anvil values; two arrays also say where they part,
/// which may be past what is written of them.
fn compared(x: &Value, y: &Value, ty: &Ty, program: &Program) -> String {
    let mut out = format!("  left:  {}\n  right: {}", show(x, ty, program), show(y, ty, program));
    if let (Value::Array(a), Value::Array(b), Ty::Array(elem)) = (x, y, ty) {
        match a.iter().zip(b.iter()).position(|(p, q)| p != q) {
            Some(i) => out.push_str(&format!(
                "\n  first difference at element {i}: {} and {}",
                show(&a[i], elem, program),
                show(&b[i], elem, program)
            )),
            None if a.len() != b.len() => out.push_str(&format!("\n  lengths {} and {}", a.len(), b.len())),
            None => {}
        }
    }
    out
}

/// The value a read of a local, or of fields of one, names, where it is.
#[inline]
fn peek<'f>(e: &Expr, frame: &'f [Value]) -> Option<&'f Value> {
    match &e.kind {
        ExprKind::Local(l) => Some(&frame[*l]),
        ExprKind::Field(base, i) => match peek(base, frame)? {
            Value::Struct(fields) => Some(&fields[*i]),
            v => unreachable!("field of {v:?}"),
        },
        _ => None,
    }
}

/// The text a literal or a read in place names, where it is.
#[inline]
fn text_at<'f>(e: &'f Expr, frame: &'f [Value]) -> Option<&'f str> {
    match &e.kind {
        ExprKind::Str(s) => Some(s),
        _ => match peek(e, frame) {
            Some(Value::Str(s)) => Some(s),
            _ => None,
        },
    }
}

fn length_of(v: &Value) -> i64 {
    match v {
        Value::Str(s) => s.len() as i64,
        Value::Bytes(b) => b.len() as i64,
        Value::Array(items) => items.len() as i64,
        Value::Map(entries) => entries.len() as i64,
        other => unreachable!("len of {other:?}"),
    }
}

/// Whether `e` reads a place inside a local that can be walked to in place, copying nothing on the
/// way: fields and indices that change no local, so when they are evaluated cannot be seen. A
/// local read for the last time is not one: it moves, which copies nothing either.
fn is_chain(e: &Expr) -> bool {
    let mut cur = e;
    loop {
        match &cur.kind {
            ExprKind::Local(_) => return true,
            ExprKind::Field(base, _) | ExprKind::Index { base, plain: true, .. } => cur = base,
            _ => return false,
        }
    }
}

/// The value chain `e` names, given its indices from the local outwards (checked already), and how
/// many of them it used.
fn walk<'f>(e: &Expr, frame: &'f [Value], indices: &[i64]) -> (&'f Value, usize) {
    match &e.kind {
        ExprKind::Local(l) => (&frame[*l], 0),
        ExprKind::Field(base, i) => match walk(base, frame, indices) {
            (Value::Struct(fields), n) => (&fields[*i], n),
            (v, _) => unreachable!("field of {v:?}"),
        },
        ExprKind::Index { base, .. } => match walk(base, frame, indices) {
            (Value::Array(items), n) => (&items[indices[n] as usize], n + 1),
            (v, _) => unreachable!("index of {v:?}"),
        },
        other => unreachable!("a chain through {other:?}"),
    }
}

pub struct Interp<'p, 'h> {
    program: &'p Program,
    out: &'p mut dyn Write,
    host: Host<'h>,
    /// Reads of the clock so far, and when the run began (the real clock counts from there).
    clock_reads: i64,
    started: std::time::Instant,
    random: Random,
    /// Files written in this run when writes are kept in memory, by `memory_file_key`.
    written: HashMap<String, Rc<[u8]>>,
    /// Running a test, where `exit` is a failure.
    in_test: bool,
    depth: usize,
    /// Statements and loop iterations left before the run stops; None is unlimited.
    steps_left: Option<u64>,
    /// Steps taken so far.
    taken: u64,
    /// Bytes allocated so far, by the model of `alloc_bytes`.
    allocated: u64,
    /// Of those, the bytes copied because a value was still shared when it was changed.
    copied: u64,
    rec: Option<&'p mut Recorder>,
    /// The texts with room to grow in place, as the runtime's growable text (made by appends),
    /// by the address of their bytes. Each Weak keeps its address from being reused while it is
    /// listed; dead ones are dropped when the list doubles.
    growable: ByAddress<Grown>,
    grown_limit: usize,
    /// Each text literal, made once, by the address of its text in the program; and the address of
    /// what was made, whose one extra reference here means nothing to a program.
    literals: ByAddress<Rc<str>>,
    literal_texts: HashSet<usize, BuildHasherDefault<AddressHash>>,
    /// Frames of finished calls, to use again.
    frames: Vec<Vec<Value>>,
    /// What the `return` running now returns.
    returned: Value,
    /// What a failed `assert` on arrays, structs and the like compared.
    sides: Option<String>,
}

/// A text with room to grow (see `Interp::growable`).
enum Grown {
    Str(Weak<str>),
    Bytes(Weak<[u8]>),
}

impl Grown {
    fn alive(&self) -> bool {
        match self {
            Grown::Str(w) => w.strong_count() > 0,
            Grown::Bytes(w) => w.strong_count() > 0,
        }
    }
}

/// The address of a text's bytes, and how many references it has and its length.
fn text_of(v: &Value) -> (usize, usize, u64) {
    match v {
        Value::Str(s) => (Rc::as_ptr(s) as *const u8 as usize, Rc::strong_count(s), s.len() as u64),
        Value::Bytes(b) => (Rc::as_ptr(b) as *const u8 as usize, Rc::strong_count(b), b.len() as u64),
        other => unreachable!("text of {other:?}"),
    }
}

/// `a` with `parts` after it: str or bytes as `a` is.
fn joined(a: &Value, parts: &[Value]) -> Value {
    match a {
        Value::Bytes(b) => {
            let mut all = b.to_vec();
            for p in parts {
                if let Value::Bytes(x) = p {
                    all.extend_from_slice(x);
                }
            }
            Value::Bytes(all.into())
        }
        old => {
            let mut all = old.as_str().to_string();
            for p in parts {
                all.push_str(p.as_str());
            }
            Value::Str(all.into())
        }
    }
}

/// What a run did, for `profile`, `trace` and coverage. Counts are always kept; the timeline only
/// when `trace` is set. Everything here is deterministic: steps, not time.
pub struct Recorder {
    /// Steps the whole run took, the bytes it allocated (by the model of `made_bytes`), and of
    /// those the bytes it copied because a value was still shared when it was changed.
    pub steps: u64,
    pub bytes: u64,
    pub copied: u64,
    /// The same, summed over every run the recorder counted (each case of a property test).
    pub totals: [u64; 3],
    /// Indexed by FuncId.
    pub funcs: Vec<FuncStats>,
    pub trace: Option<Trace>,
    /// Active calls, innermost last.
    stack: Vec<Active>,
    /// Frames of each function on the stack, so recursion counts inclusive steps once.
    active: Vec<u32>,
    /// Runs of each branch, by BranchId.
    pub branches: Vec<u64>,
    /// The bytes copied because a value was still shared when it changed, by the site of the
    /// change (`file:line:col`).
    pub copies: BTreeMap<String, CopySite>,
    /// Whether each call keeps its site, so a copy says which call it came from (`profile`).
    pub sites: bool,
    /// Steps and bytes by source line, when the program was compiled with its lines, by index
    /// in `Program::lines`; the line running now, and what had been counted when it began.
    pub lines: Vec<[u64; 2]>,
    line: Option<usize>,
    line_from: [u64; 2],
}

/// The copies made at one site.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CopySite {
    /// The function the site is in (the first instance seen, for a generic); None in a test.
    pub func: Option<FuncId>,
    pub bytes: u64,
    pub times: u64,
    /// The same copies by the site of the call the function was running for: (bytes, times).
    /// The empty site is a run not called from the program (main, a test, `parallel_map`).
    pub from: BTreeMap<String, (u64, u64)>,
    /// When the call the function was running for moved all it was handed, the same copies by
    /// the nearest call further up that did not: what kept a second reference, as far as a call
    /// can show it.
    pub kept_up: BTreeMap<String, (u64, u64)>,
}

/// A call in progress: what had been counted when it began (steps, bytes, copied), and what its
/// finished callees counted since.
struct Active {
    func: FuncId,
    at: [u64; 3],
    callees: [u64; 3],
    /// The call's site when the recorder keeps sites, else empty.
    site: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FuncStats {
    pub calls: u64,
    /// Steps from entering to leaving, callees included (a recursive function's own nested calls
    /// are counted once, in the outermost one).
    pub steps: u64,
    /// Steps in the function's own body, callees excluded.
    pub self_steps: u64,
    /// Bytes allocated from entering to leaving, callees included (see `alloc_bytes`), and by
    /// its own body.
    pub bytes: u64,
    pub self_bytes: u64,
    /// Of `self_bytes`, those its own body copied because a value was still shared when it was
    /// changed (copy on write): what moving the value on its last read would save.
    pub self_copied: u64,
}

/// A bounded timeline of calls, returns and prints.
pub struct Trace {
    /// Functions whose calls and returns are kept (by FuncId); None keeps all. Prints are always kept.
    pub only: Option<Vec<bool>>,
    pub limit: usize,
    pub events: Vec<Event>,
    /// Events left out after `limit` was reached.
    pub dropped: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// `step` is the number of steps taken before the event; main's body runs at depth 1.
    Call {
        step: u64,
        depth: usize,
        func: FuncId,
        args: Vec<Value>,
    },
    Return {
        step: u64,
        depth: usize,
        func: FuncId,
        value: Value,
    },
    Print {
        step: u64,
        depth: usize,
        text: String,
        /// `eprint`
        stderr: bool,
    },
}

impl Recorder {
    pub fn new(program: &Program) -> Recorder {
        let n = program.funcs.len();
        Recorder {
            steps: 0,
            bytes: 0,
            copied: 0,
            totals: [0; 3],
            funcs: vec![FuncStats::default(); n],
            trace: None,
            stack: Vec::new(),
            active: vec![0; n],
            branches: vec![0; program.branches.len()],
            copies: BTreeMap::new(),
            sites: false,
            lines: vec![[0; 2]; program.lines.len()],
            line: None,
            line_from: [0; 2],
        }
    }

    /// Counts what happened since the last change of line to the line that was running.
    fn change_line(&mut self, next: Option<usize>, now: [u64; 2]) {
        if let Some(l) = self.line {
            for (total, (n, from)) in self.lines[l].iter_mut().zip(now.iter().zip(&self.line_from)) {
                *total += n - from;
            }
        }
        self.line = next;
        self.line_from = now;
    }

    /// Counts `bytes` copied at site `at`, under the call running now.
    fn copied_at(&mut self, at: &str, bytes: u64, handed: &BTreeMap<String, Vec<Handed>>) {
        let Recorder { stack, copies, .. } = self;
        let keeps = |site: &str| handed.get(site).is_some_and(|args| args.iter().any(|a| a.kept.is_some()));
        // Sites are empty unless the recorder keeps them, and nothing keeps at an empty site.
        let up = match stack.split_last() {
            Some((last, rest)) if !keeps(&last.site) => rest.iter().rev().map(|a| a.site.as_str()).find(|s| keeps(s)),
            _ => None,
        };
        let (func, from) = match stack.last() {
            Some(a) => (Some(a.func), a.site.as_str()),
            None => (None, ""),
        };
        if !copies.contains_key(at) {
            copies.insert(at.to_string(), CopySite { func, ..CopySite::default() });
        }
        let c = copies.get_mut(at).expect("inserted above");
        c.bytes += bytes;
        c.times += 1;
        if !c.from.contains_key(from) {
            c.from.insert(from.to_string(), (0, 0));
        }
        let f = c.from.get_mut(from).expect("inserted above");
        f.0 += bytes;
        f.1 += 1;
        if let Some(up) = up {
            let k = c.kept_up.entry(up.to_string()).or_default();
            k.0 += bytes;
            k.1 += 1;
        }
    }

    /// A recorder that also keeps a timeline of at most `limit` events, of the functions in
    /// `only` (all when None).
    pub fn tracing(program: &Program, only: Option<Vec<bool>>, limit: usize) -> Recorder {
        let mut r = Recorder::new(program);
        r.trace = Some(Trace { only, limit, events: Vec::new(), dropped: 0 });
        r
    }

    fn event(&mut self, func: Option<FuncId>, make: impl FnOnce() -> Event) {
        let Some(t) = &mut self.trace else { return };
        if let (Some(f), Some(only)) = (func, &t.only) {
            if !only[f] {
                return;
            }
        }
        if t.events.len() < t.limit {
            t.events.push(make());
        } else {
            t.dropped += 1;
        }
    }

    /// `now` is what has been counted so far: steps taken, bytes allocated, bytes copied.
    /// `site` is the call's (empty when it is not a call in the program).
    fn enter(&mut self, func: FuncId, args: &[Value], now: [u64; 3], depth: usize, site: &str) {
        let taken = now[0];
        self.funcs[func].calls += 1;
        let site = if self.sites { site.to_string() } else { String::new() };
        self.stack.push(Active { func, at: now, callees: [0; 3], site });
        self.active[func] += 1;
        self.event(Some(func), || Event::Call { step: taken, depth, func, args: args.to_vec() });
    }

    fn leave(&mut self, result: Option<&Value>, now: [u64; 3], depth: usize) {
        let Active { func, at, callees, .. } = self.stack.pop().expect("leave without enter");
        let taken = now[0];
        let inclusive: [u64; 3] = std::array::from_fn(|k| now[k] - at[k]);
        self.funcs[func].self_steps += inclusive[0] - callees[0];
        self.funcs[func].self_bytes += inclusive[1] - callees[1];
        self.funcs[func].self_copied += inclusive[2] - callees[2];
        self.active[func] -= 1;
        if self.active[func] == 0 {
            self.funcs[func].steps += inclusive[0];
            self.funcs[func].bytes += inclusive[1];
        }
        if let Some(parent) = self.stack.last_mut() {
            for (callee, n) in parent.callees.iter_mut().zip(&inclusive) {
                *callee += n;
            }
        }
        if let Some(v) = result {
            self.event(Some(func), || Event::Return { step: taken, depth, func, value: v.clone() });
        }
    }
}

/// How a run ended. Exit codes match the native executable: main's value, or 101 on panic.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Exit(i32),
    Panicked(String),
    /// The step limit ran out; this is the tool stopping the program, not the program failing.
    OutOfSteps(u64),
}

pub fn step_limit_message(limit: u64) -> String {
    format!("stopped after {limit} steps (the step limit); the program may loop forever")
}

/// Runs main. Needs a large stack for deep recursion; see `run_on_big_stack`. A step is one
/// statement or one loop iteration; `max_steps` None runs without a limit.
pub fn run_main(program: &Program, out: &mut dyn Write, max_steps: Option<u64>, host: Host) -> Outcome {
    run_main_recorded(program, out, max_steps, None, host)
}

impl<'p, 'h> Interp<'p, 'h> {
    fn new(
        program: &'p Program,
        out: &'p mut dyn Write,
        max_steps: Option<u64>,
        rec: Option<&'p mut Recorder>,
        host: Host<'h>,
    ) -> Interp<'p, 'h> {
        Interp {
            program,
            out,
            host,
            clock_reads: 0,
            started: std::time::Instant::now(),
            random: Random::default(),
            written: HashMap::new(),
            in_test: false,
            depth: 0,
            steps_left: max_steps,
            taken: 0,
            allocated: 0,
            copied: 0,
            rec,
            growable: ByAddress::default(),
            grown_limit: 1024,
            literals: ByAddress::default(),
            literal_texts: HashSet::default(),
            frames: Vec::new(),
            returned: Value::Void,
            sides: None,
        }
    }
}

/// `run_main`, counting into `rec` when given.
pub fn run_main_recorded(
    program: &Program,
    out: &mut dyn Write,
    max_steps: Option<u64>,
    rec: Option<&mut Recorder>,
    host: Host,
) -> Outcome {
    let main = program.main.expect("run_main needs a main function");
    let mut it = Interp::new(program, out, max_steps, rec, host);
    let result = it.call(main, Vec::new(), "");
    it.finish();
    match result.map_err(|u| *u) {
        Ok(Value::Void) => Outcome::Exit(0),
        Ok(Value::I64(code)) if (0..=255).contains(&code) => Outcome::Exit(code as i32),
        Ok(Value::I64(code)) => Outcome::Panicked(main_exit_message(code)),
        Ok(other) => unreachable!("main returned {other:?}"),
        Err(Unwind::Panic(m)) => Outcome::Panicked(m),
        Err(Unwind::OutOfSteps) => Outcome::OutOfSteps(max_steps.unwrap()),
        Err(Unwind::Exit(code)) => Outcome::Exit(code),
        Err(Unwind::Return(v)) => unreachable!("`?` escaped main: {v:?}"),
    }
}

/// Runs one test body. Ok(()) if it finished, Err(message) if it panicked or ran out of steps.
pub fn run_test(
    program: &Program,
    index: usize,
    out: &mut dyn Write,
    max_steps: Option<u64>,
    host: Host,
) -> Result<(), String> {
    run_test_recorded(program, index, out, max_steps, None, host)
}

/// `run_test`, counting into `rec` when given. The test body itself belongs to no function.
pub fn run_test_recorded(
    program: &Program,
    index: usize,
    out: &mut dyn Write,
    max_steps: Option<u64>,
    rec: Option<&mut Recorder>,
    host: Host,
) -> Result<(), String> {
    run_test_with(program, index, &[], out, max_steps, rec, host).map_err(|f| match f {
        TestFail::Panic(m, _) | TestFail::Stopped(m) => m,
    })
}

/// Why a test run did not finish.
#[derive(Clone, Debug, PartialEq)]
pub enum TestFail {
    /// The text after "panic: ", and what a failed `assert` on arrays, structs and the like compared.
    Panic(String, Option<String>),
    /// The step limit's message.
    Stopped(String),
}

/// Runs one test body with `inputs` bound to its `for all` inputs (none for a plain test). Each
/// run starts its own clock and random sequence.
pub fn run_test_with(
    program: &Program,
    index: usize,
    inputs: &[Value],
    out: &mut dyn Write,
    max_steps: Option<u64>,
    rec: Option<&mut Recorder>,
    host: Host,
) -> Result<(), TestFail> {
    let test = &program.tests[index];
    let mut it = Interp::new(program, out, max_steps, rec, host);
    it.in_test = true;
    let mut frame = vec![Value::Void; test.locals.len()];
    for (l, v) in test.inputs.iter().zip(inputs) {
        frame[*l] = v.clone();
    }
    let r = it.block(&test.body, &mut frame);
    it.finish();
    let sides = it.sides.take();
    match r.map_err(|u| *u) {
        Ok(_) => Ok(()),
        Err(Unwind::Panic(m)) => Err(TestFail::Panic(m, sides)),
        Err(Unwind::OutOfSteps) => Err(TestFail::Stopped(step_limit_message(max_steps.unwrap()))),
        Err(Unwind::Exit(code)) => unreachable!("exit({code}) in a test panics instead"),
        Err(Unwind::Return(v)) => unreachable!("`?` in a test: {v:?}"),
    }
}

/// How a property test went.
#[derive(Debug)]
pub enum Property {
    Passed {
        cases: u64,
    },
    /// The first failing case, after shrinking: its inputs, its panic and what it printed.
    Failed {
        case: u64,
        inputs: Vec<Value>,
        message: String,
        output: String,
        shrinks: u64,
    },
    Stopped(String),
}

/// Shrinking stops after this many tries, whatever it has reached.
pub const MAX_SHRINKS: u64 = 1000;

/// Runs property test `index` on `cases` generated inputs (seeded by its name and `seed`, so
/// every run sees the same cases); a failing case is shrunk while it keeps failing.
pub fn run_property(
    program: &Program,
    index: usize,
    cases: u64,
    seed: u64,
    max_steps: Option<u64>,
    mut rec: Option<&mut Recorder>,
    mut host: Host,
) -> Property {
    let test = &program.tests[index];
    let types: Vec<Ty> = test.inputs.iter().map(|l| test.locals[*l].ty.clone()).collect();
    let mut rng = gen::Rng::new(gen::test_seed(&test.name, seed));
    for case in 0..cases {
        // Small inputs first, larger as the cases go on.
        let size = 1 + case * 30 / cases.max(1);
        let inputs: Vec<Value> = types.iter().map(|t| gen::generate(t, program, &mut rng, size)).collect();
        let mut out = Vec::new();
        match run_test_with(program, index, &inputs, &mut out, max_steps, rec.as_deref_mut(), host.reborrow()) {
            Ok(()) => {}
            Err(TestFail::Stopped(m)) => return Property::Stopped(m),
            Err(TestFail::Panic(message, _)) => {
                let (inputs, message, output, shrinks) =
                    shrink_failure(program, index, &types, (inputs, message, out), max_steps, host);
                return Property::Failed { case, inputs, message, output, shrinks };
            }
        }
    }
    Property::Passed { cases }
}

/// Greedily replaces one input at a time with a smaller one that still fails.
fn shrink_failure(
    program: &Program,
    index: usize,
    types: &[Ty],
    (mut inputs, mut message, out): (Vec<Value>, String, Vec<u8>),
    max_steps: Option<u64>,
    mut host: Host,
) -> (Vec<Value>, String, String, u64) {
    let mut output = String::from_utf8_lossy(&out).into_owned();
    let mut tries = 0;
    let mut kept = 0;
    'smaller: loop {
        for i in 0..inputs.len() {
            for c in gen::shrink(&inputs[i], &types[i], program) {
                if tries == MAX_SHRINKS {
                    break 'smaller;
                }
                tries += 1;
                let mut trial = inputs.clone();
                trial[i] = c;
                let mut out = Vec::new();
                let run = run_test_with(program, index, &trial, &mut out, max_steps, None, host.reborrow());
                if let Err(TestFail::Panic(m, _)) = run {
                    inputs = trial;
                    message = m;
                    output = String::from_utf8_lossy(&out).into_owned();
                    kept += 1;
                    continue 'smaller;
                }
            }
        }
        break;
    }
    (inputs, message, output, kept)
}

/// How one call of a function went, when checking its contract on generated arguments.
#[derive(Clone, Debug, PartialEq)]
pub enum Called {
    /// It returned, with its ensures holding: what it used.
    Returned(Usage),
    /// Its requires do not hold for the arguments, so the call says nothing about it.
    Excluded,
    /// Integer arithmetic overflowed: the arguments are beyond what i64 holds for this work,
    /// which the language itself refuses, so the call says nothing about the contract either.
    Overflowed,
    /// Its steps bound for the arguments is more than the check may run, so it is not run: the
    /// function says it may take that long.
    Beyond,
    /// It panicked with its requires met: an ensures failed, or something in its body.
    Panicked(String),
    Stopped,
}

/// What one call used, and its bounds for its arguments when it has them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Usage {
    pub steps: u64,
    pub steps_bound: Option<u64>,
    /// Bytes allocated, by the model of `made_bytes`, callees included.
    pub bytes: u64,
    pub bytes_bound: Option<u64>,
}

/// Calls function `fid` with `args`: its requires first on their own (so a call they exclude is
/// told apart from one that breaks the contract), then the call itself, which checks its ensures.
/// A steps bound is evaluated from the arguments before the call, which may then take that many
/// steps and no more.
pub fn call_checked(program: &Program, fid: FuncId, args: &[Value], max_steps: Option<u64>, host: Host) -> Called {
    call_bounded(program, fid, args, max_steps, host, true)
}

/// `call_checked`, or with `enforce` false, the bounds computed and reported but not held to:
/// what the function uses, bound or not, for `measure`.
fn call_bounded(
    program: &Program,
    fid: FuncId,
    args: &[Value],
    max_steps: Option<u64>,
    host: Host,
    enforce: bool,
) -> Called {
    let mut out = Vec::new();
    let mut it = Interp::new(program, &mut out, max_steps, None, host);
    let f = &program.funcs[fid];
    let mut frame = vec![Value::Void; f.locals.len()];
    for (p, a) in f.params.iter().zip(args) {
        frame[*p] = a.clone();
    }
    let outcome = |r: R<Value>| match r.map_err(|u| *u) {
        Ok(_) => unreachable!("a return is measured where the call is made"),
        Err(Unwind::Panic(m)) if is_overflow(&m) => Called::Overflowed,
        Err(Unwind::Panic(m)) => Called::Panicked(m),
        Err(Unwind::OutOfSteps) => Called::Stopped,
        Err(other) => unreachable!("{other:?} out of a pure function"),
    };
    for c in &f.requires {
        match it.eval(&c.expr, &mut frame) {
            Ok(v) if v.as_bool() => {}
            Ok(_) => return Called::Excluded,
            Err(e) => return outcome(Err(e)),
        }
    }
    let mut bound = None;
    if let Some(c) = &f.steps {
        match it.eval(&c.expr, &mut frame) {
            Ok(v) => bound = Some((v.as_i64().max(0) as u64, c)),
            Err(e) => return outcome(Err(e)),
        }
    }
    let mut alloc_bound = None;
    if let Some(c) = &f.allocs {
        match it.eval(&c.expr, &mut frame) {
            Ok(v) => alloc_bound = Some((v.as_i64().max(0) as u64, c)),
            Err(e) => return outcome(Err(e)),
        }
    }
    if let Some((b, _)) = bound.filter(|_| enforce) {
        if it.steps_left.is_some_and(|left| b >= left) {
            return Called::Beyond;
        }
        let cap = b.saturating_add(1);
        it.steps_left = Some(it.steps_left.map_or(cap, |left| left.min(cap)));
    }
    let (before, allocated_before) = (it.taken, it.allocated);
    let r = it.call(fid, args.to_vec(), "");
    let steps = it.taken - before;
    let bytes = it.allocated - allocated_before;
    let (bound_enforced, alloc_enforced) = if enforce { (bound, alloc_bound) } else { (None, None) };
    match (r, bound_enforced) {
        // The cap lets a call take one step past its bound, to tell the two apart: one that
        // returns on that step broke it too.
        (Ok(_), Some((b, c))) if steps > b => {
            Called::Panicked(format!("takes more than {b} steps, past {}", c.message))
        }
        (Ok(_), _) => match alloc_enforced {
            Some((b, c)) if bytes > b => {
                Called::Panicked(format!("allocates {bytes} bytes, more than {b}, past {}", c.message))
            }
            _ => Called::Returned(Usage {
                steps,
                steps_bound: bound.map(|(b, _)| b),
                bytes,
                bytes_bound: alloc_bound.map(|(b, _)| b),
            }),
        },
        // Stopped by the bound, not by the check's own limit: one step past it.
        (Err(u), Some((b, c))) if *u == Unwind::OutOfSteps && steps > b => {
            Called::Panicked(format!("takes more than {b} steps, past {}", c.message))
        }
        (r, _) => outcome(r),
    }
}

/// The largest size measured: arrays, maps and text up to 40 long, integers up to 400.
pub const COST_SIZE: u64 = 40;

/// Calls pure function `fid` on `cases` generated arguments of growing size (seeded by its name,
/// arrays and text as long as the size allows) and hands each call that returned to `sample`,
/// with what it used and its bounds, which are not held to here: a call over its bound is
/// measured too. Calls its requires exclude, or that overflow, panic or run out of steps, are
/// left out.
pub fn measure(
    program: &Program,
    fid: FuncId,
    cases: u64,
    max_steps: Option<u64>,
    mut host: Host,
    mut sample: impl FnMut(&[Value], Usage),
) {
    let f = &program.funcs[fid];
    let types: Vec<Ty> = f.params.iter().map(|p| f.locals[*p].ty.clone()).collect();
    let mut rng = gen::Rng::new(gen::test_seed(&f.name, 1));
    for case in 0..cases {
        let size = case * COST_SIZE / cases.max(1);
        let mut inputs: Vec<Value> =
            types.iter().map(|t| gen::generate_with(t, program, &mut rng, size, true)).collect();
        let mut called = call_bounded(program, fid, &inputs, max_steps, host.reborrow(), false);
        // Requires that tie sizes together rarely hold by chance.
        if matches!(called, Called::Excluded) {
            gen::fit_lengths(&mut inputs);
            gen::fit_numbers(&mut inputs);
            called = call_bounded(program, fid, &inputs, max_steps, host.reborrow(), false);
        }
        if let Called::Returned(usage) = called {
            sample(&inputs, usage);
        }
    }
}

/// What checking a contract found.
#[derive(Debug)]
pub enum Contract {
    /// No generated arguments broke it: `checked` met its requires, `excluded` did not,
    /// `overflowed` overflowed i64, and `beyond` were not run, their steps bound being more than
    /// the check may run.
    Held { checked: u64, excluded: u64, overflowed: u64, beyond: u64 },
    /// The smallest arguments found (after `shrinks` steps) that meet its requires and make it
    /// panic, with the panic.
    Broken { inputs: Vec<Value>, message: String, shrinks: u64 },
    /// Arguments on which it ran out of steps.
    Stopped { inputs: Vec<Value> },
}

/// Checks the contract of pure function `fid` on `cases` generated arguments (seeded by its name
/// and `seed`): with its requires met, it must return, its ensures holding. A breaking case is
/// shrunk while it keeps breaking.
pub fn check_contract(
    program: &Program,
    fid: FuncId,
    cases: u64,
    seed: u64,
    max_steps: Option<u64>,
    mut host: Host,
) -> Contract {
    let f = &program.funcs[fid];
    let types: Vec<Ty> = f.params.iter().map(|p| f.locals[*p].ty.clone()).collect();
    let mut rng = gen::Rng::new(gen::test_seed(&f.name, seed));
    // A steps or allocs bound is a claim about growth, which short arrays cannot show: up to 40 items.
    let wide = f.steps.is_some() || f.allocs.is_some();
    let largest = if wide { COST_SIZE } else { 30 };
    let (mut checked, mut excluded, mut overflowed, mut beyond) = (0, 0, 0, 0);
    // Requires that tie arguments together rarely hold by chance, so if too few inputs got past
    // them, more try arrays of one length and small numbers, which take that length as their limit.
    let more = 4 * cases;
    for case in 0..cases + more {
        if case == cases && checked >= cases / 10 {
            break;
        }
        let size = 1 + case.min(cases) * largest / cases.max(1);
        let mut inputs: Vec<Value> =
            types.iter().map(|t| gen::generate_with(t, program, &mut rng, size, wide)).collect();
        if case >= cases {
            gen::fit_lengths(&mut inputs);
            gen::fit_numbers(&mut inputs);
        }
        match call_checked(program, fid, &inputs, max_steps, host.reborrow()) {
            Called::Returned(_) => checked += 1,
            Called::Excluded => excluded += 1,
            Called::Overflowed => overflowed += 1,
            Called::Beyond => beyond += 1,
            Called::Stopped => return Contract::Stopped { inputs },
            Called::Panicked(mut message) => {
                let mut inputs = inputs;
                let (mut tries, mut kept) = (0, 0);
                'smaller: loop {
                    for i in 0..inputs.len() {
                        for c in gen::shrink(&inputs[i], &types[i], program) {
                            if tries == MAX_SHRINKS {
                                break 'smaller;
                            }
                            tries += 1;
                            let mut trial = inputs.clone();
                            trial[i] = c;
                            if let Called::Panicked(m) = call_checked(program, fid, &trial, max_steps, host.reborrow())
                            {
                                (inputs, message) = (trial, m);
                                kept += 1;
                                continue 'smaller;
                            }
                        }
                    }
                    break;
                }
                return Contract::Broken { inputs, message, shrinks: kept };
            }
        }
    }
    Contract::Held { checked, excluded, overflowed, beyond }
}

/// Whether values of `ty` can be generated: everything but function types (and what holds them).
pub fn generatable(ty: &Ty, program: &Program) -> bool {
    fn go(ty: &Ty, program: &Program, seen: &mut Vec<String>) -> bool {
        match ty {
            Ty::I64 | Ty::F64(_) | Ty::Bool | Ty::Str | Ty::Bytes => true,
            Ty::Array(t) => go(t, program, seen),
            Ty::Map(k, v) => go(k, program, seen) && go(v, program, seen),
            Ty::Struct(id) | Ty::Enum(id) => {
                let key = format!("{ty:?}");
                if seen.contains(&key) {
                    return true;
                }
                seen.push(key);
                match ty {
                    Ty::Struct(_) => program.structs[*id].fields.iter().all(|(_, t)| go(t, program, seen)),
                    _ => program.enums[*id].variants.iter().all(|v| v.fields.iter().all(|(_, t)| go(t, program, seen))),
                }
            }
            Ty::Void | Ty::Fn(_) | Ty::Param(_) | Ty::Error => false,
        }
    }
    go(ty, program, &mut Vec::new())
}

/// Runs `f` on a thread with a stack big enough for MAX_CALL_DEPTH interpreted calls.
pub fn run_on_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(f)
        .expect("cannot spawn interpreter thread")
        .join()
        .expect("interpreter thread crashed")
}

/// Text of a value for print and interpolation; floats with a unit get " unit" appended.
pub fn print_text(v: &Value, ty: &Ty) -> String {
    match (v, ty) {
        (Value::I64(x), _) => x.to_string(),
        (Value::F64(x), Ty::F64(u)) if !u.is_dimensionless() => format!("{} {u}", format_f64(*x)),
        (Value::F64(x), _) => format_f64(*x),
        (Value::Bool(b), _) => b.to_string(),
        (Value::Str(s), _) => s.to_string(),
        (v, _) => unreachable!("text of {v:?}"),
    }
}

/// What writing to `items` in place copies first: all of it, `per` bytes an element, when the
/// storage is shared (value semantics), else nothing.
fn copy_bytes<T>(items: &Rc<Vec<T>>, per: u64) -> u64 {
    if Rc::strong_count(items) > 1 {
        per * items.len() as u64
    } else {
        0
    }
}

/// The bytes a value just made holds, by the model the tools count with: a str or bytes, its
/// length; an array, 8 an element; a map, 16 an entry; a closure, 8 a captured value. Options,
/// results and structs are held inline, so only what they hold counts. With `deep`, the
/// elements of an array count too (each a new value: the parts of `split`, say).
fn made_bytes(v: &Value, deep: bool) -> u64 {
    match v {
        Value::Str(s) => s.len() as u64,
        Value::Bytes(b) => b.len() as u64,
        Value::Array(items) => {
            8 * items.len() as u64 + if deep { items.iter().map(|x| made_bytes(x, false)).sum() } else { 0 }
        }
        Value::Map(entries) => 16 * entries.len() as u64,
        Value::Fn(c) => 8 * c.captures.len() as u64,
        Value::Struct(fields) | Value::Enum(_, fields) => fields.iter().map(|f| made_bytes(f, deep)).sum(),
        Value::Void | Value::I64(_) | Value::F64(_) | Value::Bool(_) => 0,
    }
}

/// What evaluating `e` (to `v`) allocated itself, by `made_bytes`: only the expressions that
/// make a new str, bytes, array, map or closure (the ones `noalloc` refuses). Literal text is
/// static, and what subexpressions and calls allocate is counted where they run.
fn alloc_bytes(e: &Expr, v: &Value) -> u64 {
    match &e.kind {
        ExprKind::ArrayLit(_)
        | ExprKind::MapLit { .. }
        | ExprKind::Slice { .. }
        | ExprKind::Interp(_)
        | ExprKind::Closure { .. } => made_bytes(v, false),
        // An append counts itself (`Interp::append`).
        ExprKind::StrConcat(..) if !concat_appends(e) => made_bytes(v, false),
        ExprKind::Builtin(b, _) => match b {
            Builtin::StrSplit { .. } | Builtin::StrChars | Builtin::Args | Builtin::ListDir => made_bytes(v, true),
            Builtin::MapKeys
            | Builtin::MapValues
            | Builtin::ReadLine { .. }
            | Builtin::ReadStdin
            | Builtin::ReadFile
            | Builtin::StrTrim
            | Builtin::BytesOf { .. }
            | Builtin::ReadBytes
            | Builtin::BytesToStr
            | Builtin::BytesHex
            | Builtin::BytesToArray
            | Builtin::StrReplace { .. }
            | Builtin::StrUpper
            | Builtin::StrLower
            | Builtin::ArrJoin
            | Builtin::FloatFixed { .. }
            | Builtin::ArrSorted { .. }
            | Builtin::ArrReversed
            | Builtin::Env
            | Builtin::ParallelMap => made_bytes(v, false),
            _ => 0,
        },
        _ => 0,
    }
}

impl Interp<'_, '_> {
    /// The value at `path` inside `slot`, ready to be changed. Array storage on the way is made
    /// unique first, so other copies of the array never see the change.
    fn place_mut<'v>(&mut self, mut slot: &'v mut Value, path: &[Step], indices: &[i64]) -> R<&'v mut Value> {
        let mut next_index = indices.iter();
        for step in path {
            slot = match step {
                Step::Field(i) => match slot {
                    Value::Struct(fields) => &mut fields.make_mut()[*i],
                    v => unreachable!("field of {v:?}"),
                },
                Step::Index { site, .. } => {
                    let i = *next_index.next().unwrap();
                    match slot {
                        Value::Array(items) => {
                            self.allocated += self.copy_of(items, 8, site);
                            let items = Rc::make_mut(items);
                            let len = items.len() as i64;
                            if i < 0 || i >= len {
                                return panic(index_message(i, len, site));
                            }
                            &mut items[i as usize]
                        }
                        v => unreachable!("index of {v:?}"),
                    }
                }
            };
        }
        Ok(slot)
    }
}

impl<'p, 'h> Interp<'p, 'h> {
    /// The text literal `s`, made the first time it is read.
    fn literal(&mut self, s: &str) -> Value {
        let at = s.as_ptr() as usize;
        if let Some(text) = self.literals.get(&at) {
            return Value::Str(text.clone());
        }
        let text: Rc<str> = s.into();
        self.literal_texts.insert(Rc::as_ptr(&text) as *const u8 as usize);
        self.literals.insert(at, text.clone());
        Value::Str(text)
    }

    /// Evaluates the indices of chain `e`, checking each, in the order a copy on the way would
    /// have evaluated them.
    fn chain_indices(&mut self, e: &Expr, frame: &mut [Value], indices: &mut Indices) -> R<()> {
        match &e.kind {
            ExprKind::Field(base, _) => self.chain_indices(base, frame, indices),
            ExprKind::Index { base, index, site, .. } => {
                self.chain_indices(base, frame, indices)?;
                let i = self.int(index, frame)?;
                let len = match walk(base, frame, indices).0 {
                    Value::Array(items) => items.len() as i64,
                    Value::Bytes(b) => b.len() as i64,
                    other => unreachable!("index of {other:?}"),
                };
                if i < 0 || i >= len {
                    return panic(index_message(i, len, site));
                }
                indices.push(i);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Evaluates an i64, without going through a value when it is at hand.
    fn int(&mut self, e: &Expr, frame: &mut [Value]) -> R<i64> {
        match &e.kind {
            ExprKind::Int(v) => Ok(*v),
            // A move leaves the number behind, which nothing reads.
            ExprKind::Local(l) | ExprKind::Move(l) => Ok(frame[*l].as_i64()),
            ExprKind::Field(..) if peek(e, frame).is_some() => Ok(peek(e, frame).unwrap().as_i64()),
            ExprKind::Index { base, index, site, plain: true } if peek(base, frame).is_some() => {
                let i = self.int(index, frame)?;
                match peek(base, frame).unwrap() {
                    Value::Array(items) if (i as u64) < items.len() as u64 => Ok(items[i as usize].as_i64()),
                    Value::Bytes(b) if (i as u64) < b.len() as u64 => Ok(b[i as usize] as i64),
                    Value::Array(items) => panic(index_message(i, items.len() as i64, site)),
                    Value::Bytes(b) => panic(index_message(i, b.len() as i64, site)),
                    other => unreachable!("index of {other:?}"),
                }
            }
            ExprKind::IArith { op, l, r, overflow, div_zero } => {
                let a = self.int(l, frame)?;
                let b = self.int(r, frame)?;
                let v = match op {
                    IOp::Add => a.checked_add(b),
                    IOp::Sub => a.checked_sub(b),
                    IOp::Mul => a.checked_mul(b),
                    IOp::Div | IOp::Rem => {
                        if b == 0 {
                            return panic(div_zero.clone());
                        }
                        if *op == IOp::Div {
                            a.checked_div(b)
                        } else {
                            a.checked_rem(b)
                        }
                    }
                };
                match v {
                    Some(v) => Ok(v),
                    None => panic(overflow.clone()),
                }
            }
            ExprKind::Bits { op, l, r, site } => {
                let (a, n) = (self.int(l, frame)?, self.int(r, frame)?);
                let shift = matches!(op, BitOp::Shl | BitOp::Shr | BitOp::UShr);
                if shift && !(0..=63).contains(&n) {
                    return panic(shift_message(n, site));
                }
                Ok(match op {
                    BitOp::And => a & n,
                    BitOp::Or => a | n,
                    BitOp::Xor => a ^ n,
                    BitOp::Shl => ((a as u64) << n) as i64,
                    BitOp::Shr => a >> n,
                    BitOp::UShr => ((a as u64) >> n) as i64,
                })
            }
            ExprKind::BitNot(x) => Ok(!self.int(x, frame)?),
            ExprKind::Builtin(Builtin::Len, args) if peek(&args[0], frame).is_some() => {
                Ok(length_of(peek(&args[0], frame).unwrap()))
            }
            _ => Ok(self.eval(e, frame)?.as_i64()),
        }
    }

    /// Evaluates an f64, likewise.
    fn float(&mut self, e: &Expr, frame: &mut [Value]) -> R<f64> {
        match &e.kind {
            ExprKind::Float(v) => Ok(*v),
            ExprKind::Local(l) | ExprKind::Move(l) => Ok(frame[*l].as_f64()),
            ExprKind::Field(..) if peek(e, frame).is_some() => Ok(peek(e, frame).unwrap().as_f64()),
            ExprKind::FArith(op, l, r) => {
                let a = self.float(l, frame)?;
                let b = self.float(r, frame)?;
                Ok(match op {
                    FOp::Add => a + b,
                    FOp::Sub => a - b,
                    FOp::Mul => a * b,
                    FOp::Div => a / b,
                })
            }
            ExprKind::FNeg(x) => Ok(-self.float(x, frame)?),
            _ => Ok(self.eval(e, frame)?.as_f64()),
        }
    }

    /// Evaluates a bool, likewise.
    fn truth(&mut self, e: &Expr, frame: &mut [Value]) -> R<bool> {
        match &e.kind {
            ExprKind::Bool(v) => Ok(*v),
            ExprKind::Local(l) | ExprKind::Move(l) => Ok(frame[*l].as_bool()),
            ExprKind::Field(..) if peek(e, frame).is_some() => Ok(peek(e, frame).unwrap().as_bool()),
            ExprKind::Cmp(op, l, r) if matches!(l.ty, Ty::I64) => {
                let a = self.int(l, frame)?;
                let b = self.int(r, frame)?;
                Ok(cmp(*op, &a, &b))
            }
            ExprKind::Cmp(op, l, r) if matches!(l.ty, Ty::F64(_)) => {
                let a = self.float(l, frame)?;
                let b = self.float(r, frame)?;
                Ok(cmp(*op, &a, &b))
            }
            ExprKind::Cmp(op, l, r) if matches!(l.ty, Ty::Str) => match (text_at(l, frame), text_at(r, frame)) {
                (Some(a), Some(b)) => Ok(cmp(*op, a.as_bytes(), b.as_bytes())),
                _ => {
                    let a = self.eval(l, frame)?;
                    let b = self.eval(r, frame)?;
                    Ok(cmp(*op, a.as_str().as_bytes(), b.as_str().as_bytes()))
                }
            },
            ExprKind::And(l, r) => Ok(self.truth(l, frame)? && self.truth(r, frame)?),
            ExprKind::Or(l, r) => Ok(self.truth(l, frame)? || self.truth(r, frame)?),
            ExprKind::Not(x) => Ok(!self.truth(x, frame)?),
            _ => Ok(self.eval(e, frame)?.as_bool()),
        }
    }

    fn step(&mut self) -> R<()> {
        if let Some(n) = &mut self.steps_left {
            if *n == 0 {
                return Err(Box::new(Unwind::OutOfSteps));
            }
            *n -= 1;
        }
        self.taken += 1;
        Ok(())
    }

    /// Flushes output and closes the calls a panic or the step limit left open.
    fn finish(&mut self) {
        let _ = self.out.flush();
        if let Some(r) = &mut self.rec {
            while !r.stack.is_empty() {
                r.leave(None, [self.taken, self.allocated, self.copied], 0);
            }
            r.change_line(None, [self.taken, self.allocated]);
            r.steps = self.taken;
            r.bytes = self.allocated;
            r.copied = self.copied;
            for (t, n) in r.totals.iter_mut().zip([self.taken, self.allocated, self.copied]) {
                *t += n;
            }
        }
    }

    /// The line running now, when lines are being counted.
    fn line_now(&self) -> Option<usize> {
        self.rec.as_ref().and_then(|r| r.line)
    }

    /// `line` runs from here on.
    fn at_line(&mut self, line: Option<usize>) {
        if let Some(r) = &mut self.rec {
            r.change_line(line, [self.taken, self.allocated]);
        }
    }

    fn is_growable(&self, v: &Value) -> bool {
        self.growable.contains_key(&text_of(v).0)
    }

    /// Lists text `v` as having room to grow (it was just made by an append).
    fn mark_growable(&mut self, v: &Value) {
        if self.growable.len() >= self.grown_limit {
            self.growable.retain(|_, g| g.alive());
            self.grown_limit = (self.growable.len() * 2).max(1024);
        }
        let g = match v {
            Value::Str(s) => Grown::Str(Rc::downgrade(s)),
            Value::Bytes(b) => Grown::Bytes(Rc::downgrade(b)),
            other => unreachable!("growable {other:?}"),
        };
        self.growable.insert(text_of(v).0, g);
    }

    /// Text converted between str and bytes is the same object in the runtime: it keeps its
    /// room when the old value is gone, and loses it while both share it.
    fn converted(&mut self, from: &Value, to: &Value) {
        let (at, refs, _) = text_of(from);
        if self.growable.contains_key(&at) {
            if refs == 1 {
                self.mark_growable(to);
            } else {
                self.growable.remove(&at);
            }
        }
    }

    /// Counts appending `parts` to text `a` as the runtime does it, and gives the text: text
    /// that is the only reference and has room grows in place (its room doubling when it runs
    /// out); other text is copied into new room first, a copy at `site` when it was shared.
    fn append(&mut self, a: &Value, parts: &[Value], site: &str) -> Value {
        let (at, refs, mut len) = text_of(a);
        let mut grown = refs == 1 && self.is_growable(a);
        if !grown && refs - self.literal_texts.contains(&at) as usize > 1 {
            self.note_copy(site, len);
        }
        for p in parts {
            let k = text_of(p).2;
            if !grown || len + k + 1 > text_room(len) {
                self.allocated += len + k;
                grown = true;
            } else {
                self.allocated += k;
            }
            len += k;
        }
        let v = joined(a, parts);
        self.mark_growable(&v);
        v
    }

    /// `copy_bytes`, counted as copied at site `at` too.
    fn copy_of<T>(&mut self, items: &Rc<Vec<T>>, per: u64, at: &str) -> u64 {
        let c = copy_bytes(items, per);
        self.note_copy(at, c);
        c
    }

    /// Counts `bytes` copied at site `at` because a value was still shared when it changed.
    fn note_copy(&mut self, at: &str, bytes: u64) {
        if bytes == 0 {
            return;
        }
        self.copied += bytes;
        if let Some(r) = &mut self.rec {
            r.copied_at(at, bytes, &self.program.handed);
        }
    }

    /// Calls `fid`; `site` is the call's (empty when it is not a call in the program).
    fn call(&mut self, fid: FuncId, args: Vec<Value>, site: &str) -> R<Value> {
        if self.depth >= MAX_CALL_DEPTH {
            return panic("stack overflow: calls nested too deeply");
        }
        self.depth += 1;
        if let Some(r) = &mut self.rec {
            r.enter(fid, &args, [self.taken, self.allocated, self.copied], self.depth, site);
        }
        let line = self.line_now();
        let result = self.call_body(fid, args);
        self.at_line(line);
        if let (Some(r), Ok(v)) = (&mut self.rec, &result) {
            r.leave(Some(v), [self.taken, self.allocated, self.copied], self.depth);
        }
        self.depth -= 1;
        result
    }

    fn call_body(&mut self, fid: FuncId, args: Vec<Value>) -> R<Value> {
        let f = &self.program.funcs[fid];
        if f.external.is_some() {
            let bits: Vec<u64> = args
                .iter()
                .map(|a| match a {
                    Value::I64(i) => *i as u64,
                    Value::F64(x) => x.to_bits(),
                    other => unreachable!("{other:?} passed to C"),
                })
                .collect();
            let Some(foreign) = self.host.foreign.as_deref_mut() else {
                unreachable!("`{}` is an extern function, but the run has no C library", f.name)
            };
            let r = foreign.call(fid, &bits);
            return Ok(match f.ret {
                Ty::Void => Value::Void,
                Ty::I64 => Value::I64(r as i64),
                Ty::F64(_) => Value::F64(f64::from_bits(r)),
                ref t => unreachable!("C returned {t:?}"),
            });
        }
        debug_assert_eq!(f.params.len(), args.len(), "`{}`", f.name);
        let mut frame = if f.captures.is_some() {
            // What a function literal captured it meets after its own parameters.
            let mut frame = vec![Value::Void; f.locals.len()];
            for (p, a) in f.params.iter().zip(args) {
                frame[*p] = a;
            }
            frame
        } else {
            // The arguments are the first locals, so they begin the frame.
            debug_assert!(f.params.iter().enumerate().all(|(i, p)| i == *p), "`{}`", f.name);
            let mut frame = args;
            frame.resize(f.locals.len(), Value::Void);
            frame
        };
        for c in &f.requires {
            if !self.eval(&c.expr, &mut frame)?.as_bool() {
                return panic(c.message.clone());
            }
        }
        let ret = match self.block(&f.body, &mut frame) {
            Ok(Flow::Return) => std::mem::replace(&mut self.returned, Value::Void),
            Ok(_) => Value::Void,
            Err(u) => match *u {
                Unwind::Return(v) => v,
                other => return Err(Box::new(other)),
            },
        };
        if let Some(r) = f.result_local {
            frame[r] = ret.clone();
        }
        for c in &f.ensures {
            if !self.eval(&c.expr, &mut frame)?.as_bool() {
                return panic(c.message.clone());
            }
        }
        frame.clear();
        self.frames.push(frame);
        Ok(ret)
    }

    fn block(&mut self, stmts: &[Stmt], frame: &mut [Value]) -> R<Flow> {
        for s in stmts {
            match self.stmt(s, frame)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    /// Evaluates the index expressions of a place, left to right.
    fn indices(&mut self, path: &[Step], frame: &mut [Value]) -> R<Indices> {
        let mut out = Indices::new();
        for step in path {
            if let Step::Index { index, .. } = step {
                out.push(self.int(index, frame)?);
            }
        }
        Ok(out)
    }

    /// Runs a loop body; Some(flow) when the loop must stop.
    fn loop_body(&mut self, body: &[Stmt], frame: &mut [Value]) -> R<Option<Flow>> {
        self.step()?;
        Ok(match self.block(body, frame)? {
            Flow::Break => Some(Flow::Normal),
            Flow::Return => Some(Flow::Return),
            Flow::Normal | Flow::Continue => None,
        })
    }

    fn stmt(&mut self, s: &Stmt, frame: &mut [Value]) -> R<Flow> {
        match s {
            Stmt::Reached(id) => {
                if let Some(rec) = &mut self.rec {
                    rec.branches[*id] += 1;
                }
                return Ok(Flow::Normal);
            }
            Stmt::Drop(l) => {
                frame[*l] = Value::Void;
                return Ok(Flow::Normal);
            }
            Stmt::Line(id) => {
                self.at_line(Some(*id));
                return Ok(Flow::Normal);
            }
            _ => {}
        }
        self.step()?;
        match s {
            Stmt::Let(l, e) => {
                frame[*l] = self.eval(e, frame)?;
            }
            Stmt::Assign(l, path, e, _) if path.is_empty() && !matches!(e.kind, ExprKind::StrConcat(..)) => {
                frame[*l] = self.eval(e, frame)?;
            }
            // `s = s + a + b`: the same text as concatenating, counted as the native append is.
            Stmt::Assign(l, path, e, site) if path.is_empty() && append_parts(*l, e).is_some() => {
                let mut parts = Vec::new();
                for p in append_parts(*l, e).unwrap() {
                    parts.push(self.eval(p, frame)?);
                }
                let old = std::mem::replace(&mut frame[*l], Value::Void);
                frame[*l] = self.append(&old, &parts, site);
            }
            Stmt::Assign(l, path, e, _) => {
                let v = self.eval(e, frame)?;
                let indices = self.indices(path, frame)?;
                *self.place_mut(&mut frame[*l], path, &indices)? = v;
            }
            Stmt::Expr(e) => {
                self.eval(e, frame)?;
            }
            Stmt::If(c, t, e) => {
                let branch = if self.truth(c, frame)? { t } else { e };
                return self.block(branch, frame);
            }
            Stmt::While(c, body) => {
                let here = self.line_now();
                while self.truth(c, frame)? {
                    if let Some(flow) = self.loop_body(body, frame)? {
                        return Ok(flow);
                    }
                    self.at_line(here);
                }
            }
            Stmt::For(l, start, end, body) => {
                let s = self.eval(start, frame)?.as_i64();
                let e = self.eval(end, frame)?.as_i64();
                let mut i = s;
                let here = self.line_now();
                while i < e {
                    frame[*l] = Value::I64(i);
                    self.at_line(here);
                    if let Some(flow) = self.loop_body(body, frame)? {
                        return Ok(flow);
                    }
                    i += 1;
                }
            }
            Stmt::ForEach { var, held, iter, body } => {
                let items = self.eval(iter, frame)?;
                let Value::Array(a) = &items else { unreachable!("for over {items:?}") };
                let n = a.len();
                frame[*held] = items;
                let here = self.line_now();
                for i in 0..n {
                    let Value::Array(a) = &mut frame[*held] else { unreachable!("the snapshot is never written") };
                    // An array nothing else holds hands its elements out, so they stay unshared.
                    let item = match Rc::get_mut(a) {
                        Some(items) => std::mem::replace(&mut items[i], Value::Void),
                        None => a[i].clone(),
                    };
                    frame[*var] = item;
                    self.at_line(here);
                    if let Some(flow) = self.loop_body(body, frame)? {
                        return Ok(flow);
                    }
                }
            }
            Stmt::Match { held, scrutinee, arms } => {
                // `held` is let go of at the start of each arm (see `moves`) and nothing reads it, so
                // here it never holds anything.
                let _ = held;
                if is_chain(scrutinee) {
                    let mut indices = Indices::new();
                    self.chain_indices(scrutinee, frame, &mut indices)?;
                    let Value::Enum(tag, _) = walk(scrutinee, frame, &indices).0 else {
                        unreachable!("match on something else")
                    };
                    let tag = *tag;
                    let arm =
                        arms.iter().find(|a| a.variant == tag || a.also.contains(&tag)).expect("match is exhaustive");
                    for (field, local) in &arm.bindings {
                        let Value::Enum(_, fields) = walk(scrutinee, frame, &indices).0 else { unreachable!() };
                        frame[*local] = fields[*field].clone();
                    }
                    return self.block(&arm.body, frame);
                }
                let v = self.eval(scrutinee, frame)?;
                let Value::Enum(tag, fields) = v else { unreachable!("match on {v:?}") };
                let arm = arms.iter().find(|a| a.variant == tag || a.also.contains(&tag)).expect("match is exhaustive");
                for (field, local) in &arm.bindings {
                    frame[*local] = fields[*field].clone();
                }
                // Holding on to these through the arm would keep them shared.
                drop(fields);
                return self.block(&arm.body, frame);
            }
            Stmt::Return(v) => {
                self.returned = match v {
                    Some(e) => self.eval(e, frame)?,
                    None => Value::Void,
                };
                return Ok(Flow::Return);
            }
            Stmt::Break => return Ok(Flow::Break),
            Stmt::Continue => return Ok(Flow::Continue),
            Stmt::Reached(_) | Stmt::Drop(_) | Stmt::Line(_) => unreachable!("handled above"),
        }
        Ok(Flow::Normal)
    }

    /// Evaluates `e`, counting what it allocates.
    fn eval(&mut self, e: &Expr, frame: &mut [Value]) -> R<Value> {
        // What runs most and allocates nothing goes straight there: `eval_inner` has a large frame.
        match &e.kind {
            ExprKind::Local(l) => return Ok(frame[*l].clone()),
            ExprKind::Int(v) => return Ok(Value::I64(*v)),
            ExprKind::Move(l) => return Ok(std::mem::replace(&mut frame[*l], Value::Void)),
            ExprKind::IArith { .. } | ExprKind::Bits { .. } => return Ok(Value::I64(self.int(e, frame)?)),
            ExprKind::Cmp(_, l, _) if matches!(l.ty, Ty::I64 | Ty::F64(_) | Ty::Str) => {
                return Ok(Value::Bool(self.truth(e, frame)?))
            }
            ExprKind::And(..) | ExprKind::Or(..) | ExprKind::Not(_) => return Ok(Value::Bool(self.truth(e, frame)?)),
            ExprKind::Field(..) if peek(e, frame).is_some() => return Ok(peek(e, frame).unwrap().clone()),
            ExprKind::Call(fid, args, site) => {
                let mut vals = self.frames.pop().unwrap_or_default();
                self.eval_args(args, frame, &mut vals)?;
                return self.call(*fid, vals, site);
            }
            ExprKind::Index { base, index, site, plain: true } if peek(base, frame).is_some() => {
                let i = self.int(index, frame)?;
                return match peek(base, frame).unwrap() {
                    Value::Array(items) if (i as u64) < items.len() as u64 => Ok(items[i as usize].clone()),
                    Value::Bytes(b) if (i as u64) < b.len() as u64 => Ok(Value::I64(b[i as usize] as i64)),
                    Value::Array(items) => panic(index_message(i, items.len() as i64, site)),
                    Value::Bytes(b) => panic(index_message(i, b.len() as i64, site)),
                    other => unreachable!("index of {other:?}"),
                };
            }
            ExprKind::StructLit(_, fields) => return Ok(Value::Struct(self.eval_fields(fields, frame)?)),
            ExprKind::Variant { variant, fields, .. } => {
                return Ok(Value::Enum(*variant, self.eval_fields(fields, frame)?))
            }
            ExprKind::Str(s) => return Ok(self.literal(s)),
            ExprKind::Builtin(Builtin::Len, args) if peek(&args[0], frame).is_some() => {
                return Ok(Value::I64(length_of(peek(&args[0], frame).unwrap())));
            }
            _ => {}
        }
        let v = self.eval_inner(e, frame)?;
        self.allocated += alloc_bytes(e, &v);
        Ok(v)
    }

    #[inline(never)]
    fn eval_inner(&mut self, e: &Expr, frame: &mut [Value]) -> R<Value> {
        Ok(match &e.kind {
            ExprKind::Int(_) => unreachable!("handled in `eval`"),
            ExprKind::Float(v) => Value::F64(*v),
            ExprKind::Bool(v) => Value::Bool(*v),
            ExprKind::Str(_)
            | ExprKind::Local(_)
            | ExprKind::Move(_)
            | ExprKind::Call(..)
            | ExprKind::StructLit(..)
            | ExprKind::Variant { .. }
            | ExprKind::IArith { .. }
            | ExprKind::Bits { .. } => unreachable!("handled in `eval`"),
            // The last read of a place: taken out of it, reached as a write to it would be.
            ExprKind::Take { local, path } => {
                let indices = self.indices(path, frame)?;
                let slot = self.place_mut(&mut frame[*local], path, &indices)?;
                std::mem::replace(slot, Value::Void)
            }
            ExprKind::Field(..) if is_chain(e) => {
                let mut indices = Indices::new();
                self.chain_indices(e, frame, &mut indices)?;
                walk(e, frame, &indices).0.clone()
            }
            ExprKind::Field(base, i) => match self.eval(base, frame)? {
                Value::Struct(fields) => fields[*i].clone(),
                v => unreachable!("field of {v:?}"),
            },
            ExprKind::FnRef(fid) => Value::Fn(Rc::new(Closure { func: *fid, captures: Vec::new() })),
            ExprKind::Closure { func, captures } => {
                Value::Fn(Rc::new(Closure { func: *func, captures: self.eval_all(captures, frame)? }))
            }
            ExprKind::CallValue(callee, args, site) => {
                let Value::Fn(c) = self.eval(callee, frame)? else {
                    unreachable!("calling a value that is not a function")
                };
                let mut vals = self.frames.pop().unwrap_or_default();
                vals.extend_from_slice(&c.captures);
                self.eval_args(args, frame, &mut vals)?;
                self.call(c.func, vals, site)?
            }
            ExprKind::ArrayLit(elems) => Value::Array(Rc::new(self.eval_all(elems, frame)?)),
            ExprKind::Slice { base, from, to, site } => {
                let v = self.eval(base, frame)?;
                let f = self.eval(from, frame)?.as_i64();
                let t = self.eval(to, frame)?.as_i64();
                let len = match &v {
                    Value::Array(items) => items.len() as i64,
                    Value::Str(s) => s.len() as i64,
                    Value::Bytes(b) => b.len() as i64,
                    other => unreachable!("slice of {other:?}"),
                };
                if f < 0 || t > len || f > t {
                    return panic(slice_message(f, t, len, site));
                }
                let (f, t) = (f as usize, t as usize);
                match v {
                    Value::Array(items) => Value::Array(Rc::new(items[f..t].to_vec())),
                    Value::Bytes(b) => Value::Bytes(b[f..t].into()),
                    Value::Str(s) if s.is_char_boundary(f) && s.is_char_boundary(t) => Value::Str(s[f..t].into()),
                    _ => return panic(slice_char_message(f as i64, t as i64, site)),
                }
            }
            ExprKind::MapLit { entries, duplicate } => {
                let mut out: Vec<(Value, Value)> = Vec::with_capacity(entries.len());
                for (k, v) in entries {
                    let key = self.eval(k, frame)?;
                    let value = self.eval(v, frame)?;
                    match find_key(&out, &key) {
                        Ok(_) => return panic(duplicate.clone()),
                        Err(at) => out.insert(at, (key, value)),
                    }
                }
                Value::Map(Rc::new(out))
            }
            ExprKind::MapSet { local, path, key, value, at } => {
                let k = self.eval(key, frame)?;
                let v = self.eval(value, frame)?;
                let indices = self.indices(path, frame)?;
                match self.place_mut(&mut frame[*local], path, &indices)? {
                    Value::Map(entries) => {
                        self.allocated += self.copy_of(entries, 16, at);
                        let entries = Rc::make_mut(entries);
                        match find_key(entries, &k) {
                            Ok(at) => entries[at].1 = v,
                            Err(at) => {
                                self.allocated += 16;
                                entries.insert(at, (k, v));
                            }
                        }
                    }
                    other => unreachable!("set on {other:?}"),
                }
                Value::Void
            }
            ExprKind::MapRemove { local, path, key, missing, at: at_site } => {
                let k = self.eval(key, frame)?;
                let indices = self.indices(path, frame)?;
                match self.place_mut(&mut frame[*local], path, &indices)? {
                    Value::Map(entries) => match find_key(entries, &k) {
                        Ok(at) => {
                            self.allocated += self.copy_of(entries, 16, at_site);
                            Rc::make_mut(entries).remove(at);
                        }
                        Err(_) => return panic(missing.clone()),
                    },
                    other => unreachable!("remove on {other:?}"),
                }
                Value::Void
            }
            ExprKind::Index { base, .. } if is_chain(e) => {
                let mut indices = Indices::new();
                self.chain_indices(e, frame, &mut indices)?;
                let (items, n) = walk(base, frame, &indices);
                let i = indices[n] as usize;
                match items {
                    Value::Array(items) => items[i].clone(),
                    Value::Bytes(b) => Value::I64(b[i] as i64),
                    other => unreachable!("index of {other:?}"),
                }
            }
            ExprKind::Index { base, index, site, .. } => {
                let items = self.eval(base, frame)?;
                let i = self.eval(index, frame)?.as_i64();
                let len = match &items {
                    Value::Array(items) => items.len() as i64,
                    Value::Bytes(b) => b.len() as i64,
                    other => unreachable!("index of {other:?}"),
                };
                if i < 0 || i >= len {
                    return panic(index_message(i, len, site));
                }
                match items {
                    Value::Array(items) => items[i as usize].clone(),
                    Value::Bytes(b) => Value::I64(b[i as usize] as i64),
                    _ => unreachable!(),
                }
            }
            ExprKind::Interp(parts) => {
                let mut text = String::new();
                for p in parts {
                    let v = self.eval(p, frame)?;
                    text.push_str(&print_text(&v, &p.ty));
                }
                Value::Str(text.into())
            }
            // A value handed over is appended to, as `s = s + x` is (ir::concat_appends).
            ExprKind::StrConcat(l, r, site) if concat_appends(e) => {
                let a = self.eval(l, frame)?;
                let b = self.eval(r, frame)?;
                self.append(&a, &[b], site)
            }
            ExprKind::StrConcat(l, r, _) => match (self.eval(l, frame)?, self.eval(r, frame)?) {
                (Value::Bytes(a), Value::Bytes(b)) => Value::Bytes([&a[..], &b[..]].concat().into()),
                (a, b) => Value::Str(format!("{}{}", a.as_str(), b.as_str()).into()),
            },
            ExprKind::Push { local, path, value, at } => {
                let v = self.eval(value, frame)?;
                let indices = self.indices(path, frame)?;
                match self.place_mut(&mut frame[*local], path, &indices)? {
                    Value::Array(items) => {
                        self.allocated += self.copy_of(items, 8, at) + 8;
                        Rc::make_mut(items).push(v);
                    }
                    other => unreachable!("push to {other:?}"),
                }
                Value::Void
            }
            ExprKind::Pop { local, path, message, at } => {
                let indices = self.indices(path, frame)?;
                match self.place_mut(&mut frame[*local], path, &indices)? {
                    Value::Array(items) => {
                        self.allocated += self.copy_of(items, 8, at);
                        match Rc::make_mut(items).pop() {
                            Some(v) => v,
                            None => return panic(message.clone()),
                        }
                    }
                    other => unreachable!("pop from {other:?}"),
                }
            }
            ExprKind::Try { held, inner, .. } => {
                // Nothing reads `held`, and it is let go of before anything could tell.
                let _ = held;
                match self.eval(inner, frame)? {
                    Value::Enum(0, fields) => fields[0].clone(),
                    // The failure variant (None or Err) has the same index and fields in the return type.
                    Value::Enum(failure, fields) => return Err(Box::new(Unwind::Return(Value::Enum(failure, fields)))),
                    other => unreachable!("`?` on {other:?}"),
                }
            }
            ExprKind::BitNot(_) => Value::I64(self.int(e, frame)?),
            ExprKind::FArith(..) | ExprKind::FNeg(_) => Value::F64(self.float(e, frame)?),
            ExprKind::Cmp(_, l, _) if matches!(l.ty, Ty::I64 | Ty::F64(_) | Ty::Str) => {
                Value::Bool(self.truth(e, frame)?)
            }
            ExprKind::Cmp(op, l, r) => {
                let a = self.eval(l, frame)?;
                let b = self.eval(r, frame)?;
                Value::Bool(match (&a, &b) {
                    (Value::Bool(x), Value::Bool(y)) => cmp(*op, x, y),
                    // Arrays, structs, enums and maps have == and != only.
                    _ if *op == CmpOp::Eq => a == b,
                    _ if *op == CmpOp::Ne => a != b,
                    _ => unreachable!("compare {a:?} {b:?}"),
                })
            }
            ExprKind::Cond { cond, then, otherwise, then_branch, else_branch } => {
                let (branch, id) =
                    if self.truth(cond, frame)? { (then, then_branch) } else { (otherwise, else_branch) };
                if let (Some(id), Some(rec)) = (id, &mut self.rec) {
                    rec.branches[*id] += 1;
                }
                self.eval(branch, frame)?
            }
            ExprKind::And(..) | ExprKind::Or(..) | ExprKind::Not(_) => Value::Bool(self.truth(e, frame)?),
            ExprKind::INeg { e, overflow } => match self.eval(e, frame)?.as_i64().checked_neg() {
                Some(v) => Value::I64(v),
                None => return panic(overflow.clone()),
            },
            ExprKind::Builtin(b, args) => self.builtin(b, args, frame)?,
        })
    }

    fn eval_fields(&mut self, es: &[Expr], frame: &mut [Value]) -> R<Fields> {
        if es.is_empty() {
            return Ok(Fields::default());
        }
        let mut vals = self.frames.pop().unwrap_or_default();
        for e in es {
            vals.push(self.eval(e, frame)?);
        }
        let fields = Fields(Some(vals.drain(..).collect()));
        self.frames.push(vals);
        Ok(fields)
    }

    fn eval_all(&mut self, es: &[Expr], frame: &mut [Value]) -> R<Vec<Value>> {
        let mut vals = Vec::with_capacity(es.len());
        for e in es {
            vals.push(self.eval(e, frame)?);
        }
        Ok(vals)
    }

    /// A call's arguments, those handed over last (see `arg_comes_last`).
    fn eval_args(&mut self, es: &[Expr], frame: &mut [Value], vals: &mut Vec<Value>) -> R<()> {
        let start = vals.len();
        vals.resize(start + es.len(), Value::Void);
        let mut later = false;
        for (e, v) in es.iter().zip(&mut vals[start..]) {
            if arg_comes_last(e) {
                later = true;
            } else {
                *v = self.eval(e, frame)?;
            }
        }
        if later {
            for (e, v) in es.iter().zip(&mut vals[start..]) {
                if arg_comes_last(e) {
                    *v = self.eval(e, frame)?;
                }
            }
        }
        Ok(())
    }

    #[inline(never)]
    fn builtin(&mut self, b: &Builtin, args: &[Expr], frame: &mut [Value]) -> R<Value> {
        match b {
            Builtin::Len if is_chain(&args[0]) => {
                let mut indices = Indices::new();
                self.chain_indices(&args[0], frame, &mut indices)?;
                return Ok(Value::I64(length_of(walk(&args[0], frame, &indices).0)));
            }
            Builtin::ClockNs => {
                let ns = match self.host.clock {
                    Clock::Virtual => self.clock_reads * VIRTUAL_TICK_NS,
                    Clock::Real => self.started.elapsed().as_nanos().min(i64::MAX as u128) as i64,
                };
                self.clock_reads += 1;
                return Ok(Value::I64(ns));
            }
            Builtin::RandomF64 => return Ok(Value::F64(self.random.unit_f64())),
            Builtin::Args => {
                let all = self.host.args.iter().map(|a| Value::Str(a.as_str().into())).collect();
                return Ok(Value::Array(Rc::new(all)));
            }
            Builtin::ReadLine { message } => return self.read_line(message),
            Builtin::ReadStdin => {
                let mut all = Vec::new();
                if let Some(input) = self.host.stdin.as_deref_mut() {
                    input.read_to_end(&mut all).expect("stdin read failed");
                }
                return Ok(Value::Bytes(all.into()));
            }
            _ => {}
        }
        let x = self.eval(&args[0], frame)?;
        Ok(match b {
            Builtin::ClockNs | Builtin::RandomF64 | Builtin::Args | Builtin::ReadLine { .. } | Builtin::ReadStdin => {
                unreachable!()
            }
            Builtin::ReadFile => {
                file_result(self.read_file(x.as_str()).and_then(|b| match String::from_utf8(b.to_vec()) {
                    Ok(s) => Ok(Value::Str(s.into())),
                    Err(_) => Err(FILE_NOT_TEXT),
                }))
            }
            Builtin::ReadBytes => file_result(self.read_file(x.as_str()).map(Value::Bytes)),
            Builtin::WriteFile | Builtin::WriteBytes => {
                let data = match self.eval(&args[1], frame)? {
                    Value::Str(s) => s.as_bytes().into(),
                    Value::Bytes(b) => b,
                    other => unreachable!("write {other:?}"),
                };
                file_result(self.write_file(x.as_str(), data))
            }
            Builtin::WrappingAdd | Builtin::WrappingSub | Builtin::WrappingMul => {
                let (a, c) = (x.as_i64(), self.int(&args[1], frame)?);
                Value::I64(match b {
                    Builtin::WrappingAdd => a.wrapping_add(c),
                    Builtin::WrappingSub => a.wrapping_sub(c),
                    _ => a.wrapping_mul(c),
                })
            }
            Builtin::BitLen => Value::I64(64 - (x.as_i64() as u64).leading_zeros() as i64),
            Builtin::LeadingZeros => Value::I64((x.as_i64() as u64).leading_zeros() as i64),
            Builtin::TrailingZeros => Value::I64((x.as_i64() as u64).trailing_zeros() as i64),
            Builtin::BytesOf { site } => {
                let Value::Array(items) = x else { unreachable!("bytes_of {x:?}") };
                let mut out = Vec::with_capacity(items.len());
                for v in items.iter() {
                    let v = v.as_i64();
                    if !(0..=255).contains(&v) {
                        return panic(byte_message(v, site));
                    }
                    out.push(v as u8);
                }
                Value::Bytes(out.into())
            }
            Builtin::StrToBytes => {
                let v = Value::Bytes(x.as_str().as_bytes().into());
                self.converted(&x, &v);
                v
            }
            Builtin::StrReplace { message } => {
                let old = self.eval(&args[1], frame)?;
                let new = self.eval(&args[2], frame)?;
                if old.as_str().is_empty() {
                    return panic(message.clone());
                }
                Value::Str(x.as_str().replace(old.as_str(), new.as_str()).into())
            }
            Builtin::StrUpper => Value::Str(case_text(x.as_str(), true).into()),
            Builtin::StrLower => Value::Str(case_text(x.as_str(), false).into()),
            Builtin::StrChars => {
                Value::Array(Rc::new(x.as_str().chars().map(|c| Value::Str(c.to_string().into())).collect()))
            }
            Builtin::ArrJoin => {
                let sep = self.eval(&args[1], frame)?;
                let Value::Array(items) = x else { unreachable!() };
                let parts: Vec<&str> = items.iter().map(|v| v.as_str()).collect();
                Value::Str(parts.join(sep.as_str()).into())
            }
            Builtin::FloatFixed { message } => {
                let n = self.eval(&args[1], frame)?.as_i64();
                if !(0..=17).contains(&n) {
                    return panic(message.clone());
                }
                Value::Str(fixed_text(x.as_f64(), n as usize).into())
            }
            Builtin::ArrReversed => {
                let Value::Array(items) = x else { unreachable!() };
                Value::Array(Rc::new(items.iter().rev().cloned().collect()))
            }
            Builtin::ArrSorted { message } => {
                let Value::Array(items) = x else { unreachable!() };
                let mut out = (*items).clone();
                if out.iter().any(|v| matches!(v, Value::F64(f) if f.is_nan())) {
                    return panic(message.clone());
                }
                // Stable, like the runtime's merge sort, so equal values (0.0 and -0.0) keep their order.
                out.sort_by(|a, b| match (a, b) {
                    (Value::I64(x), Value::I64(y)) => x.cmp(y),
                    (Value::F64(x), Value::F64(y)) => x.partial_cmp(y).expect("no nan"),
                    (Value::Str(x), Value::Str(y)) => x.as_bytes().cmp(y.as_bytes()),
                    _ => unreachable!("sorted {a:?}"),
                });
                Value::Array(Rc::new(out))
            }
            Builtin::BytesToStr => {
                let Value::Bytes(bs) = &x else { unreachable!() };
                let v = String::from_utf8(bs.to_vec()).ok().map(|s| Value::Str(s.into()));
                if let Some(v) = &v {
                    self.converted(&x, v);
                }
                option(v)
            }
            Builtin::BytesHex => {
                let Value::Bytes(bs) = x else { unreachable!() };
                Value::Str(hex_text(&bs).into())
            }
            Builtin::BytesWordAt { site } => {
                let Value::Bytes(bs) = x else { unreachable!() };
                let at = self.int(&args[1], frame)?;
                if at < 0 || at > bs.len() as i64 - 8 {
                    return panic(word_at_message(at, bs.len() as i64, site));
                }
                let start = at as usize;
                Value::I64(i64::from_le_bytes(bs[start..start + 8].try_into().unwrap()))
            }
            Builtin::BytesToArray => {
                let Value::Bytes(bs) = x else { unreachable!() };
                Value::Array(Rc::new(bs.iter().map(|b| Value::I64(*b as i64)).collect()))
            }
            Builtin::StrTrim => Value::Str(trim_text(x.as_str()).into()),
            Builtin::StrParseI64 => option(parse_i64_text(x.as_str()).map(Value::I64)),
            Builtin::StrParseF64 => option(parse_f64_text(x.as_str()).map(Value::F64)),
            Builtin::RandomBelow { site } => {
                let n = x.as_i64();
                if n <= 0 {
                    return panic(random_below_message(n, site));
                }
                Value::I64(self.random.below(n))
            }
            Builtin::RandomSeed => {
                self.random = Random::seeded(x.as_i64());
                Value::Void
            }
            Builtin::Print | Builtin::EPrint => {
                let mut line = print_text(&x, &args[0].ty);
                let stderr = matches!(b, Builtin::EPrint);
                if let Some(r) = &mut self.rec {
                    let (step, depth) = (self.taken, self.depth);
                    r.event(None, || Event::Print { step, depth, text: line.clone(), stderr });
                }
                line.push('\n');
                if stderr {
                    self.host.stderr.write_all(line.as_bytes()).expect("stderr write failed");
                } else {
                    self.out.write_all(line.as_bytes()).expect("stdout write failed");
                }
                Value::Void
            }
            Builtin::PrintRaw => {
                let text = x.as_str().to_string();
                if let Some(r) = &mut self.rec {
                    let (step, depth) = (self.taken, self.depth);
                    r.event(None, || Event::Print { step, depth, text: text.clone(), stderr: false });
                }
                self.out.write_all(text.as_bytes()).expect("stdout write failed");
                Value::Void
            }
            Builtin::Panic { site } => return panic(format!("{} ({site})", x.as_str())),
            Builtin::Exit { site } => {
                let code = x.as_i64();
                if self.in_test {
                    return panic(exit_in_test_message(code, site));
                }
                if !(0..=255).contains(&code) {
                    return panic(exit_message(code, site));
                }
                return Err(Box::new(Unwind::Exit(code as i32)));
            }
            Builtin::Env => {
                let name = x.as_str();
                let value = if name.starts_with(RESERVED_ENV_PREFIX) {
                    None
                } else {
                    match self.host.env {
                        Env::Real => std::env::var(name).ok(),
                        Env::Given(vars) => vars.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone()),
                    }
                };
                option(value.map(|v| Value::Str(v.into())))
            }
            Builtin::ListDir => file_result(self.list_dir(x.as_str())),
            // The reference meaning: a map in order (native code runs it on worker threads).
            Builtin::ParallelMap => {
                let Value::Fn(c) = self.eval(&args[1], frame)? else { unreachable!("parallel_map of a non-function") };
                let Value::Array(items) = x else { unreachable!() };
                let mut out = Vec::with_capacity(items.len());
                for item in items.iter() {
                    let mut vals = c.captures.clone();
                    vals.push(item.clone());
                    out.push(self.call(c.func, vals, "")?);
                }
                Value::Array(Rc::new(out))
            }
            Builtin::Assert { message } => {
                if !x.as_bool() {
                    return panic(message.clone());
                }
                Value::Void
            }
            Builtin::AssertCmp { message, op } => {
                let y = self.eval(&args[1], frame)?;
                let holds = match (&x, &y) {
                    (Value::I64(a), Value::I64(c)) => cmp(*op, a, c),
                    (Value::F64(a), Value::F64(c)) => cmp(*op, a, c),
                    (Value::Bool(a), Value::Bool(c)) => cmp(*op, a, c),
                    (Value::Str(a), Value::Str(c)) => cmp(*op, a.as_bytes(), c.as_bytes()),
                    _ => (x == y) == (*op == CmpOp::Eq),
                };
                if !holds {
                    let ty = &args[0].ty;
                    if !matches!(ty, Ty::I64 | Ty::F64(_) | Ty::Bool | Ty::Str) {
                        self.sides = Some(compared(&x, &y, ty, self.program));
                        return panic(message.clone());
                    }
                    let (l, r) = (print_text(&x, ty), print_text(&y, ty));
                    return panic(anvil_front::ir::failed_sides(message, &l, &r, *ty == Ty::Str));
                }
                Value::Void
            }
            Builtin::Sqrt => Value::F64(x.as_f64().sqrt()),
            Builtin::AbsI64 { message } => match x.as_i64().checked_abs() {
                Some(v) => Value::I64(v),
                None => return panic(message.clone()),
            },
            Builtin::AbsF64 => Value::F64(x.as_f64().abs()),
            Builtin::ToF64 => Value::F64(x.as_i64() as f64),
            Builtin::ToI64 { message } => {
                let v = x.as_f64();
                if !(TO_I64_MIN..TO_I64_MAX_EXCLUSIVE).contains(&v) {
                    return panic(message.clone());
                }
                Value::I64(v as i64)
            }
            Builtin::Len => match x {
                Value::Str(s) => Value::I64(s.len() as i64),
                Value::Bytes(b) => Value::I64(b.len() as i64),
                Value::Array(items) => Value::I64(items.len() as i64),
                Value::Map(entries) => Value::I64(entries.len() as i64),
                other => unreachable!("len of {other:?}"),
            },
            Builtin::MapKeys | Builtin::MapValues => {
                let Value::Map(entries) = x else { unreachable!("keys of {x:?}") };
                let keys = matches!(b, Builtin::MapKeys);
                Value::Array(Rc::new(entries.iter().map(|(k, v)| if keys { k.clone() } else { v.clone() }).collect()))
            }
            Builtin::MapGet | Builtin::MapHas => {
                let key = self.eval(&args[1], frame)?;
                let Value::Map(entries) = x else { unreachable!("get on {x:?}") };
                let found = find_key(&entries, &key).ok();
                match (b, found) {
                    (Builtin::MapHas, f) => Value::Bool(f.is_some()),
                    (_, Some(at)) => Value::Enum(OPTION_SOME, vec![entries[at].1.clone()].into()),
                    (_, None) => Value::Enum(OPTION_NONE, Fields::default()),
                }
            }
            Builtin::StrFindFrom { site } => {
                let t = self.eval(&args[1], frame)?;
                let at = self.int(&args[2], frame)?;
                let (s, t) = (x.as_str().as_bytes(), t.as_str().as_bytes());
                if at < 0 || at > s.len() as i64 {
                    return panic(find_from_message(at, s.len() as i64, site));
                }
                let from = at as usize;
                let found = if t.is_empty() { Some(0) } else { s[from..].windows(t.len()).position(|w| w == t) };
                option(found.map(|i| Value::I64((from + i) as i64)))
            }
            Builtin::StrContains | Builtin::StrFind | Builtin::StrStartsWith | Builtin::StrEndsWith => {
                let t = self.eval(&args[1], frame)?;
                let (s, t) = (x.as_str(), t.as_str());
                match b {
                    Builtin::StrContains => Value::Bool(s.contains(t)),
                    Builtin::StrStartsWith => Value::Bool(s.starts_with(t)),
                    Builtin::StrEndsWith => Value::Bool(s.ends_with(t)),
                    _ => match s.find(t) {
                        Some(at) => Value::Enum(OPTION_SOME, vec![Value::I64(at as i64)].into()),
                        None => Value::Enum(OPTION_NONE, Fields::default()),
                    },
                }
            }
            Builtin::StrSplit { message } => {
                let sep = self.eval(&args[1], frame)?;
                if sep.as_str().is_empty() {
                    return panic(message.clone());
                }
                let parts = x.as_str().split(sep.as_str()).map(|p| Value::Str(p.into())).collect();
                Value::Array(Rc::new(parts))
            }
        })
    }
}

impl Interp<'_, '_> {
    fn read_line(&mut self, message: &str) -> R<Value> {
        let Some(input) = self.host.stdin.as_deref_mut() else {
            return Ok(option(None));
        };
        let mut line = Vec::new();
        if input.read_until(b'\n', &mut line).expect("stdin read failed") == 0 {
            return Ok(option(None));
        }
        if line.last() == Some(&b'\n') {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
        }
        match String::from_utf8(line) {
            Ok(s) => Ok(option(Some(Value::Str(s.into())))),
            Err(_) => panic(message),
        }
    }

    /// The file's bytes; read_file checks they are UTF-8.
    fn read_file(&mut self, path: &str) -> Result<Rc<[u8]>, usize> {
        // An empty path names no file, on every platform.
        if path.is_empty() {
            return Err(FILE_NOT_FOUND);
        }
        if self.host.files.memory {
            if let Some(text) = self.written.get(&memory_file_key(path)) {
                return Ok(text.clone());
            }
        }
        let p = self.host.files.root.join(path);
        if p.is_dir() {
            return Err(FILE_IS_DIRECTORY);
        }
        std::fs::read(&p).map(Rc::from).map_err(file_error)
    }

    /// The names in a directory, sorted bytewise; with writes in memory, the files written into
    /// it by this run too (a file `d/x` for the directory written `d`; `x` for `.`).
    fn list_dir(&mut self, path: &str) -> Result<Value, usize> {
        if path.is_empty() {
            return Err(FILE_NOT_FOUND);
        }
        let p = self.host.files.root.join(path);
        if p.exists() && !p.is_dir() {
            return Err(FILE_NOT_DIRECTORY);
        }
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&p).map_err(file_error)? {
            let entry = entry.map_err(file_error)?;
            names.push(entry.file_name().into_string().map_err(|_| FILE_NOT_TEXT)?);
        }
        if self.host.files.memory {
            let dir = memory_file_key(path);
            let prefix = if dir == "." { String::new() } else { format!("{}/", dir.trim_end_matches('/')) };
            for key in self.written.keys() {
                if let Some(name) = key.strip_prefix(&prefix).filter(|n| !n.is_empty() && !n.contains('/')) {
                    if !names.iter().any(|n| n == name) {
                        names.push(name.to_string());
                    }
                }
            }
        }
        names.sort();
        Ok(Value::Array(Rc::new(names.into_iter().map(|n| Value::Str(n.into())).collect())))
    }

    /// Ok of the bytes written.
    fn write_file(&mut self, path: &str, data: Rc<[u8]>) -> Result<Value, usize> {
        if path.is_empty() {
            return Err(FILE_NOT_FOUND);
        }
        let p = self.host.files.root.join(path);
        if p.is_dir() {
            return Err(FILE_IS_DIRECTORY);
        }
        if self.host.files.memory {
            // As the disk would answer: the directory it goes in must exist.
            if p.parent().is_some_and(|d| !d.as_os_str().is_empty() && !d.is_dir()) {
                return Err(FILE_NOT_FOUND);
            }
            let n = data.len();
            self.written.insert(memory_file_key(path), data);
            return Ok(Value::I64(n as i64));
        }
        std::fs::write(&p, &data).map_err(file_error)?;
        Ok(Value::I64(data.len() as i64))
    }
}

/// The FileError variant for an error from the operating system (a directory is caught first).
fn file_error(e: std::io::Error) -> usize {
    match e.kind() {
        std::io::ErrorKind::NotFound => FILE_NOT_FOUND,
        std::io::ErrorKind::PermissionDenied => FILE_DENIED,
        _ => FILE_FAILED,
    }
}

fn file_result(r: Result<Value, usize>) -> Value {
    match r {
        Ok(v) => Value::Enum(RESULT_OK, vec![v].into()),
        Err(variant) => Value::Enum(RESULT_ERR, vec![Value::Enum(variant, Fields::default())].into()),
    }
}

fn option(v: Option<Value>) -> Value {
    match v {
        Some(v) => Value::Enum(OPTION_SOME, vec![v].into()),
        None => Value::Enum(OPTION_NONE, Fields::default()),
    }
}

fn cmp<T: PartialOrd + ?Sized>(op: CmpOp, a: &T, b: &T) -> bool {
    match op {
        CmpOp::Eq => a == b,
        CmpOp::Ne => a != b,
        CmpOp::Lt => a < b,
        CmpOp::Le => a <= b,
        CmpOp::Gt => a > b,
        CmpOp::Ge => a >= b,
    }
}

impl Value {
    fn as_i64(&self) -> i64 {
        match self {
            Value::I64(v) => *v,
            v => unreachable!("expected i64, got {v:?}"),
        }
    }
    fn as_f64(&self) -> f64 {
        match self {
            Value::F64(v) => *v,
            v => unreachable!("expected f64, got {v:?}"),
        }
    }
    fn as_bool(&self) -> bool {
        match self {
            Value::Bool(v) => *v,
            v => unreachable!("expected bool, got {v:?}"),
        }
    }
    fn as_str(&self) -> &str {
        match self {
            Value::Str(s) => s,
            v => unreachable!("expected str, got {v:?}"),
        }
    }
}
