//! Reference interpreter over the typed IR. It defines what a program means; the native backend
//! is tested against it (same stdout, same panic message, same exit code).

pub mod gen;
mod show;
pub use show::{show, SHOWN_BYTES, SHOWN_CHARS, SHOWN_ITEMS};

use anvil_front::ir::*;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;
use std::rc::Rc;

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
    Struct(Vec<Value>),
    Array(Rc<Vec<Value>>),
    /// Variant index and that variant's fields.
    Enum(usize, Vec<Value>),
    /// Entries in ascending key order (i64 keys by value, str keys bytewise).
    Map(Rc<Vec<(Value, Value)>>),
    /// A function value (never compared: the checker refuses `==` on functions).
    Fn(Rc<Closure>),
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

type R<T> = Result<T, Unwind>;

fn panic<T>(message: impl Into<String>) -> R<T> {
    Err(Unwind::Panic(message.into()))
}

/// Interpreted calls deeper than this panic instead of exhausting the host stack.
pub const MAX_CALL_DEPTH: usize = 10_000;

enum Flow {
    Normal,
    Break,
    Continue,
    Return(Value),
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
    rec: Option<&'p mut Recorder>,
}

/// What a run did, for `profile`, `trace` and coverage. Counts are always kept; the timeline only
/// when `trace` is set. Everything here is deterministic: steps, not time.
pub struct Recorder {
    /// Steps the whole run took.
    pub steps: u64,
    /// Indexed by FuncId.
    pub funcs: Vec<FuncStats>,
    pub trace: Option<Trace>,
    /// Active calls: function, steps taken when it began, steps its finished callees took.
    stack: Vec<(FuncId, u64, u64)>,
    /// Frames of each function on the stack, so recursion counts inclusive steps once.
    active: Vec<u32>,
    /// Runs of each branch, by BranchId.
    pub branches: Vec<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FuncStats {
    pub calls: u64,
    /// Steps from entering to leaving, callees included (a recursive function's own nested calls
    /// are counted once, in the outermost one).
    pub steps: u64,
    /// Steps in the function's own body, callees excluded.
    pub self_steps: u64,
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
            funcs: vec![FuncStats::default(); n],
            trace: None,
            stack: Vec::new(),
            active: vec![0; n],
            branches: vec![0; program.branches.len()],
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

    fn enter(&mut self, func: FuncId, args: &[Value], taken: u64, depth: usize) {
        self.funcs[func].calls += 1;
        self.stack.push((func, taken, 0));
        self.active[func] += 1;
        self.event(Some(func), || Event::Call { step: taken, depth, func, args: args.to_vec() });
    }

    fn leave(&mut self, result: Option<&Value>, taken: u64, depth: usize) {
        let (func, start, callees) = self.stack.pop().expect("leave without enter");
        let inclusive = taken - start;
        self.funcs[func].self_steps += inclusive - callees;
        self.active[func] -= 1;
        if self.active[func] == 0 {
            self.funcs[func].steps += inclusive;
        }
        if let Some(parent) = self.stack.last_mut() {
            parent.2 += inclusive;
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
            rec,
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
    let result = it.call(main, Vec::new());
    it.finish();
    match result {
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
        TestFail::Panic(m) | TestFail::Stopped(m) => m,
    })
}

/// Why a test run did not finish.
#[derive(Clone, Debug, PartialEq)]
pub enum TestFail {
    /// The text after "panic: ".
    Panic(String),
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
    match r {
        Ok(_) => Ok(()),
        Err(Unwind::Panic(m)) => Err(TestFail::Panic(m)),
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
            Err(TestFail::Panic(message)) => {
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
                if let Err(TestFail::Panic(m)) = run {
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
    /// It returned after `steps` steps, with its ensures holding; `bound` is its steps bound for
    /// these arguments, when it has one.
    Returned { steps: u64, bound: Option<u64> },
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

/// Calls function `fid` with `args`: its requires first on their own (so a call they exclude is
/// told apart from one that breaks the contract), then the call itself, which checks its ensures.
/// A steps bound is evaluated from the arguments before the call, which may then take that many
/// steps and no more.
pub fn call_checked(program: &Program, fid: FuncId, args: &[Value], max_steps: Option<u64>, host: Host) -> Called {
    let mut out = Vec::new();
    let mut it = Interp::new(program, &mut out, max_steps, None, host);
    let f = &program.funcs[fid];
    let mut frame = vec![Value::Void; f.locals.len()];
    for (p, a) in f.params.iter().zip(args) {
        frame[*p] = a.clone();
    }
    let outcome = |r: R<Value>| match r {
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
    if let Some((b, _)) = bound {
        if it.steps_left.is_some_and(|left| b >= left) {
            return Called::Beyond;
        }
        let cap = b.saturating_add(1);
        it.steps_left = Some(it.steps_left.map_or(cap, |left| left.min(cap)));
    }
    let before = it.taken;
    let r = it.call(fid, args.to_vec());
    let steps = it.taken - before;
    match (r, bound) {
        (Ok(_), _) => Called::Returned { steps, bound: bound.map(|(b, _)| b) },
        // Stopped by the bound, not by the check's own limit: one step past it.
        (Err(Unwind::OutOfSteps), Some((b, c))) if steps > b => {
            Called::Panicked(format!("takes more than {b} steps, past {}", c.message))
        }
        (r, _) => outcome(r),
    }
}

/// The largest size measured: arrays, maps and text up to 40 long, integers up to 400.
pub const COST_SIZE: u64 = 40;

/// Calls pure function `fid` on `cases` generated arguments of growing size (seeded by its name,
/// arrays and text as long as the size allows) and hands each call that returned to `sample`,
/// with its steps and its steps bound if it has one. Calls its requires exclude, or that
/// overflow, panic or run out of steps, are left out.
pub fn measure(
    program: &Program,
    fid: FuncId,
    cases: u64,
    max_steps: Option<u64>,
    mut host: Host,
    mut sample: impl FnMut(&[Value], u64, Option<u64>),
) {
    let f = &program.funcs[fid];
    let types: Vec<Ty> = f.params.iter().map(|p| f.locals[*p].ty.clone()).collect();
    let mut rng = gen::Rng::new(gen::test_seed(&f.name, 1));
    for case in 0..cases {
        let size = case * COST_SIZE / cases.max(1);
        let inputs: Vec<Value> =
            types.iter().map(|t| gen::generate_with(t, program, &mut rng, size, true)).collect();
        if let Called::Returned { steps, bound } = call_checked(program, fid, &inputs, max_steps, host.reborrow()) {
            sample(&inputs, steps, bound);
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
    // A steps bound is a claim about growth, which short arrays cannot show: up to 40 items.
    let wide = f.steps.is_some();
    let largest = if wide { COST_SIZE } else { 30 };
    let (mut checked, mut excluded, mut overflowed, mut beyond) = (0, 0, 0, 0);
    for case in 0..cases {
        let size = 1 + case * largest / cases.max(1);
        let inputs: Vec<Value> =
            types.iter().map(|t| gen::generate_with(t, program, &mut rng, size, wide)).collect();
        match call_checked(program, fid, &inputs, max_steps, host.reborrow()) {
            Called::Returned { .. } => checked += 1,
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
                            if let Called::Panicked(m) = call_checked(program, fid, &trial, max_steps, host.reborrow()) {
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

/// The value at `path` inside `root`, ready to be changed. Array storage on the way is made
/// unique first, so other copies of the array never see the change.
fn place_mut<'v>(mut slot: &'v mut Value, path: &[Step], indices: &[i64]) -> R<&'v mut Value> {
    let mut next_index = indices.iter();
    for step in path {
        slot = match step {
            Step::Field(i) => match slot {
                Value::Struct(fields) => &mut fields[*i],
                v => unreachable!("field of {v:?}"),
            },
            Step::Index { site, .. } => {
                let i = *next_index.next().unwrap();
                match slot {
                    Value::Array(items) => {
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

impl<'p, 'h> Interp<'p, 'h> {
    fn step(&mut self) -> R<()> {
        if let Some(n) = &mut self.steps_left {
            if *n == 0 {
                return Err(Unwind::OutOfSteps);
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
                r.leave(None, self.taken, 0);
            }
            r.steps = self.taken;
        }
    }

    fn call(&mut self, fid: FuncId, args: Vec<Value>) -> R<Value> {
        if self.depth >= MAX_CALL_DEPTH {
            let name = &self.program.funcs[fid].name;
            return panic(format!("stack overflow: call depth exceeded {MAX_CALL_DEPTH} in `{name}`"));
        }
        self.depth += 1;
        if let Some(r) = &mut self.rec {
            r.enter(fid, &args, self.taken, self.depth);
        }
        let result = self.call_body(fid, args);
        if let (Some(r), Ok(v)) = (&mut self.rec, &result) {
            r.leave(Some(v), self.taken, self.depth);
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
        let mut frame = vec![Value::Void; f.locals.len()];
        for (p, a) in f.params.iter().zip(args) {
            frame[*p] = a;
        }
        for c in &f.requires {
            if !self.eval(&c.expr, &mut frame)?.as_bool() {
                return panic(c.message.clone());
            }
        }
        let ret = match self.block(&f.body, &mut frame) {
            Ok(Flow::Return(v)) | Err(Unwind::Return(v)) => v,
            Ok(_) => Value::Void,
            Err(p) => return Err(p),
        };
        if let Some(r) = f.result_local {
            frame[r] = ret.clone();
        }
        for c in &f.ensures {
            if !self.eval(&c.expr, &mut frame)?.as_bool() {
                return panic(c.message.clone());
            }
        }
        Ok(ret)
    }

    fn block(&mut self, stmts: &[Stmt], frame: &mut Vec<Value>) -> R<Flow> {
        for s in stmts {
            match self.stmt(s, frame)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    /// Evaluates the index expressions of a place, left to right.
    fn indices(&mut self, path: &[Step], frame: &mut Vec<Value>) -> R<Vec<i64>> {
        let mut out = Vec::new();
        for step in path {
            if let Step::Index { index, .. } = step {
                out.push(self.eval(index, frame)?.as_i64());
            }
        }
        Ok(out)
    }

    /// Runs a loop body; Some(flow) when the loop must stop.
    fn loop_body(&mut self, body: &[Stmt], frame: &mut Vec<Value>) -> R<Option<Flow>> {
        self.step()?;
        Ok(match self.block(body, frame)? {
            Flow::Break => Some(Flow::Normal),
            Flow::Return(v) => Some(Flow::Return(v)),
            Flow::Normal | Flow::Continue => None,
        })
    }

    fn stmt(&mut self, s: &Stmt, frame: &mut Vec<Value>) -> R<Flow> {
        if let Stmt::Reached(id) = s {
            if let Some(rec) = &mut self.rec {
                rec.branches[*id] += 1;
            }
            return Ok(Flow::Normal);
        }
        self.step()?;
        match s {
            Stmt::Let(l, e) => {
                frame[*l] = self.eval(e, frame)?;
            }
            Stmt::Assign(l, path, e) => {
                let v = self.eval(e, frame)?;
                let indices = self.indices(path, frame)?;
                *place_mut(&mut frame[*l], path, &indices)? = v;
            }
            Stmt::Expr(e) => {
                self.eval(e, frame)?;
            }
            Stmt::If(c, t, e) => {
                let branch = if self.eval(c, frame)?.as_bool() { t } else { e };
                return self.block(branch, frame);
            }
            Stmt::While(c, body) => {
                while self.eval(c, frame)?.as_bool() {
                    if let Some(flow) = self.loop_body(body, frame)? {
                        return Ok(flow);
                    }
                }
            }
            Stmt::For(l, start, end, body) => {
                let s = self.eval(start, frame)?.as_i64();
                let e = self.eval(end, frame)?.as_i64();
                let mut i = s;
                while i < e {
                    frame[*l] = Value::I64(i);
                    if let Some(flow) = self.loop_body(body, frame)? {
                        return Ok(flow);
                    }
                    i += 1;
                }
            }
            Stmt::ForEach { var, held, iter, body } => {
                let items = self.eval(iter, frame)?;
                frame[*held] = items.clone();
                let Value::Array(items) = items else { unreachable!("for over {items:?}") };
                for item in items.iter() {
                    frame[*var] = item.clone();
                    if let Some(flow) = self.loop_body(body, frame)? {
                        return Ok(flow);
                    }
                }
            }
            Stmt::Match { held, scrutinee, arms } => {
                let v = self.eval(scrutinee, frame)?;
                frame[*held] = v.clone();
                let Value::Enum(tag, fields) = v else { unreachable!("match on {v:?}") };
                let arm = arms.iter().find(|a| a.variant == tag).expect("match is exhaustive");
                for (field, local) in &arm.bindings {
                    frame[*local] = fields[*field].clone();
                }
                return self.block(&arm.body, frame);
            }
            Stmt::Return(v) => {
                let v = match v {
                    Some(e) => self.eval(e, frame)?,
                    None => Value::Void,
                };
                return Ok(Flow::Return(v));
            }
            Stmt::Break => return Ok(Flow::Break),
            Stmt::Continue => return Ok(Flow::Continue),
            Stmt::Reached(_) => unreachable!("handled above"),
        }
        Ok(Flow::Normal)
    }

    fn eval(&mut self, e: &Expr, frame: &mut Vec<Value>) -> R<Value> {
        Ok(match &e.kind {
            ExprKind::Int(v) => Value::I64(*v),
            ExprKind::Float(v) => Value::F64(*v),
            ExprKind::Bool(v) => Value::Bool(*v),
            ExprKind::Str(s) => Value::Str(s.as_str().into()),
            ExprKind::Local(l) => frame[*l].clone(),
            ExprKind::Field(base, i) => match self.eval(base, frame)? {
                Value::Struct(mut fields) => fields.swap_remove(*i),
                v => unreachable!("field of {v:?}"),
            },
            ExprKind::Call(fid, args) => {
                let vals = self.eval_all(args, frame)?;
                self.call(*fid, vals)?
            }
            ExprKind::FnRef(fid) => Value::Fn(Rc::new(Closure { func: *fid, captures: Vec::new() })),
            ExprKind::Closure { func, captures } => {
                Value::Fn(Rc::new(Closure { func: *func, captures: self.eval_all(captures, frame)? }))
            }
            ExprKind::CallValue(callee, args) => {
                let Value::Fn(c) = self.eval(callee, frame)? else {
                    unreachable!("calling a value that is not a function")
                };
                let mut vals = c.captures.clone();
                vals.extend(self.eval_all(args, frame)?);
                self.call(c.func, vals)?
            }
            ExprKind::StructLit(_, fields) => Value::Struct(self.eval_all(fields, frame)?),
            ExprKind::ArrayLit(elems) => Value::Array(Rc::new(self.eval_all(elems, frame)?)),
            ExprKind::Variant { variant, fields, .. } => Value::Enum(*variant, self.eval_all(fields, frame)?),
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
            ExprKind::MapSet { local, path, key, value } => {
                let k = self.eval(key, frame)?;
                let v = self.eval(value, frame)?;
                let indices = self.indices(path, frame)?;
                match place_mut(&mut frame[*local], path, &indices)? {
                    Value::Map(entries) => {
                        let entries = Rc::make_mut(entries);
                        match find_key(entries, &k) {
                            Ok(at) => entries[at].1 = v,
                            Err(at) => entries.insert(at, (k, v)),
                        }
                    }
                    other => unreachable!("set on {other:?}"),
                }
                Value::Void
            }
            ExprKind::MapRemove { local, path, key, missing } => {
                let k = self.eval(key, frame)?;
                let indices = self.indices(path, frame)?;
                match place_mut(&mut frame[*local], path, &indices)? {
                    Value::Map(entries) => match find_key(entries, &k) {
                        Ok(at) => {
                            Rc::make_mut(entries).remove(at);
                        }
                        Err(_) => return panic(missing.clone()),
                    },
                    other => unreachable!("remove on {other:?}"),
                }
                Value::Void
            }
            ExprKind::Index { base, index, site } => {
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
            ExprKind::StrConcat(l, r) => match (self.eval(l, frame)?, self.eval(r, frame)?) {
                (Value::Bytes(a), Value::Bytes(b)) => Value::Bytes([&a[..], &b[..]].concat().into()),
                (a, b) => Value::Str(format!("{}{}", a.as_str(), b.as_str()).into()),
            },
            ExprKind::Push { local, path, value } => {
                let v = self.eval(value, frame)?;
                let indices = self.indices(path, frame)?;
                match place_mut(&mut frame[*local], path, &indices)? {
                    Value::Array(items) => Rc::make_mut(items).push(v),
                    other => unreachable!("push to {other:?}"),
                }
                Value::Void
            }
            ExprKind::Pop { local, path, message } => {
                let indices = self.indices(path, frame)?;
                match place_mut(&mut frame[*local], path, &indices)? {
                    Value::Array(items) => match Rc::make_mut(items).pop() {
                        Some(v) => v,
                        None => return panic(message.clone()),
                    },
                    other => unreachable!("pop from {other:?}"),
                }
            }
            ExprKind::Try { held, inner, .. } => {
                let v = self.eval(inner, frame)?;
                frame[*held] = v.clone();
                match v {
                    Value::Enum(0, mut fields) => fields.swap_remove(0),
                    // The failure variant (None or Err) has the same index and fields in the return type.
                    Value::Enum(failure, fields) => return Err(Unwind::Return(Value::Enum(failure, fields))),
                    other => unreachable!("`?` on {other:?}"),
                }
            }
            ExprKind::IArith { op, l, r, overflow, div_zero } => {
                let a = self.eval(l, frame)?.as_i64();
                let b = self.eval(r, frame)?.as_i64();
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
                    Some(v) => Value::I64(v),
                    None => return panic(overflow.clone()),
                }
            }
            ExprKind::FArith(op, l, r) => {
                let a = self.eval(l, frame)?.as_f64();
                let b = self.eval(r, frame)?.as_f64();
                Value::F64(match op {
                    FOp::Add => a + b,
                    FOp::Sub => a - b,
                    FOp::Mul => a * b,
                    FOp::Div => a / b,
                })
            }
            ExprKind::Cmp(op, l, r) => {
                let a = self.eval(l, frame)?;
                let b = self.eval(r, frame)?;
                Value::Bool(match (&a, &b) {
                    (Value::I64(x), Value::I64(y)) => cmp(*op, x, y),
                    (Value::F64(x), Value::F64(y)) => cmp(*op, x, y),
                    (Value::Bool(x), Value::Bool(y)) => cmp(*op, x, y),
                    // Bytewise, like memcmp in the C runtime.
                    (Value::Str(x), Value::Str(y)) => cmp(*op, x.as_bytes(), y.as_bytes()),
                    // Arrays, structs, enums and maps have == and != only.
                    _ if *op == CmpOp::Eq => a == b,
                    _ if *op == CmpOp::Ne => a != b,
                    _ => unreachable!("compare {a:?} {b:?}"),
                })
            }
            ExprKind::And(l, r) => Value::Bool(self.eval(l, frame)?.as_bool() && self.eval(r, frame)?.as_bool()),
            ExprKind::Or(l, r) => Value::Bool(self.eval(l, frame)?.as_bool() || self.eval(r, frame)?.as_bool()),
            ExprKind::Not(x) => Value::Bool(!self.eval(x, frame)?.as_bool()),
            ExprKind::INeg { e, overflow } => match self.eval(e, frame)?.as_i64().checked_neg() {
                Some(v) => Value::I64(v),
                None => return panic(overflow.clone()),
            },
            ExprKind::FNeg(x) => Value::F64(-self.eval(x, frame)?.as_f64()),
            ExprKind::BitNot(x) => Value::I64(!self.eval(x, frame)?.as_i64()),
            ExprKind::Bits { op, l, r, site } => {
                let (a, n) = (self.eval(l, frame)?.as_i64(), self.eval(r, frame)?.as_i64());
                let shift = matches!(op, BitOp::Shl | BitOp::Shr | BitOp::UShr);
                if shift && !(0..=63).contains(&n) {
                    return panic(shift_message(n, site));
                }
                Value::I64(match op {
                    BitOp::And => a & n,
                    BitOp::Or => a | n,
                    BitOp::Xor => a ^ n,
                    BitOp::Shl => ((a as u64) << n) as i64,
                    BitOp::Shr => a >> n,
                    BitOp::UShr => ((a as u64) >> n) as i64,
                })
            }
            ExprKind::Builtin(b, args) => self.builtin(b, args, frame)?,
        })
    }

    fn eval_all(&mut self, es: &[Expr], frame: &mut Vec<Value>) -> R<Vec<Value>> {
        let mut vals = Vec::with_capacity(es.len());
        for e in es {
            vals.push(self.eval(e, frame)?);
        }
        Ok(vals)
    }

    fn builtin(&mut self, b: &Builtin, args: &[Expr], frame: &mut Vec<Value>) -> R<Value> {
        match b {
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
            _ => {}
        }
        let x = self.eval(&args[0], frame)?;
        Ok(match b {
            Builtin::ClockNs | Builtin::RandomF64 | Builtin::Args | Builtin::ReadLine { .. } => unreachable!(),
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
                let (a, c) = (x.as_i64(), self.eval(&args[1], frame)?.as_i64());
                Value::I64(match b {
                    Builtin::WrappingAdd => a.wrapping_add(c),
                    Builtin::WrappingSub => a.wrapping_sub(c),
                    _ => a.wrapping_mul(c),
                })
            }
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
            Builtin::StrToBytes => Value::Bytes(x.as_str().as_bytes().into()),
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
                let Value::Bytes(bs) = x else { unreachable!() };
                option(String::from_utf8(bs.to_vec()).ok().map(|s| Value::Str(s.into())))
            }
            Builtin::BytesHex => {
                let Value::Bytes(bs) = x else { unreachable!() };
                Value::Str(hex_text(&bs).into())
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
            Builtin::Exit { site } => {
                let code = x.as_i64();
                if self.in_test {
                    return panic(exit_in_test_message(code, site));
                }
                if !(0..=255).contains(&code) {
                    return panic(exit_message(code, site));
                }
                return Err(Unwind::Exit(code as i32));
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
                    out.push(self.call(c.func, vals)?);
                }
                Value::Array(Rc::new(out))
            }
            Builtin::Assert { message } => {
                if !x.as_bool() {
                    return panic(message.clone());
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
                    (_, Some(at)) => Value::Enum(OPTION_SOME, vec![entries[at].1.clone()]),
                    (_, None) => Value::Enum(OPTION_NONE, Vec::new()),
                }
            }
            Builtin::StrContains | Builtin::StrFind | Builtin::StrStartsWith | Builtin::StrEndsWith => {
                let t = self.eval(&args[1], frame)?;
                let (s, t) = (x.as_str(), t.as_str());
                match b {
                    Builtin::StrContains => Value::Bool(s.contains(t)),
                    Builtin::StrStartsWith => Value::Bool(s.starts_with(t)),
                    Builtin::StrEndsWith => Value::Bool(s.ends_with(t)),
                    _ => match s.find(t) {
                        Some(at) => Value::Enum(OPTION_SOME, vec![Value::I64(at as i64)]),
                        None => Value::Enum(OPTION_NONE, Vec::new()),
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
        Ok(v) => Value::Enum(RESULT_OK, vec![v]),
        Err(variant) => Value::Enum(RESULT_ERR, vec![Value::Enum(variant, Vec::new())]),
    }
}

fn option(v: Option<Value>) -> Value {
    match v {
        Some(v) => Value::Enum(OPTION_SOME, vec![v]),
        None => Value::Enum(OPTION_NONE, Vec::new()),
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
