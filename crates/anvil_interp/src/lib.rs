//! Reference interpreter over the typed IR. It defines what a program means; the native backend
//! is tested against it (same stdout, same panic message, same exit code).

use anvil_front::ir::*;
use std::io::Write;
use std::rc::Rc;

/// Arrays share storage until written (Rc::make_mut), which gives value semantics cheaply.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Void,
    I64(i64),
    F64(f64),
    Bool(bool),
    Str(Rc<str>),
    Struct(Vec<Value>),
    Array(Rc<Vec<Value>>),
    /// Variant index and that variant's fields.
    Enum(usize, Vec<Value>),
}

/// Why evaluation stopped early: a panic (the text after "panic: "), `?` returning a failure,
/// or the step limit running out.
#[derive(Debug, PartialEq)]
enum Unwind {
    Panic(String),
    Return(Value),
    OutOfSteps,
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

pub struct Interp<'p> {
    program: &'p Program,
    out: &'p mut dyn Write,
    depth: usize,
    /// Statements and loop iterations left before the run stops; None is unlimited.
    steps_left: Option<u64>,
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
pub fn run_main(program: &Program, out: &mut dyn Write, max_steps: Option<u64>) -> Outcome {
    let main = program.main.expect("run_main needs a main function");
    let mut it = Interp { program, out, depth: 0, steps_left: max_steps };
    let result = it.call(main, Vec::new());
    let _ = it.out.flush();
    match result {
        Ok(Value::Void) => Outcome::Exit(0),
        Ok(Value::I64(code)) if (0..=255).contains(&code) => Outcome::Exit(code as i32),
        Ok(Value::I64(code)) => Outcome::Panicked(main_exit_message(code)),
        Ok(other) => unreachable!("main returned {other:?}"),
        Err(Unwind::Panic(m)) => Outcome::Panicked(m),
        Err(Unwind::OutOfSteps) => Outcome::OutOfSteps(max_steps.unwrap()),
        Err(Unwind::Return(v)) => unreachable!("`?` escaped main: {v:?}"),
    }
}

/// Runs one test body. Ok(()) if it finished, Err(message) if it panicked or ran out of steps.
pub fn run_test(program: &Program, index: usize, out: &mut dyn Write, max_steps: Option<u64>) -> Result<(), String> {
    let test = &program.tests[index];
    let mut it = Interp { program, out, depth: 0, steps_left: max_steps };
    let mut frame = vec![Value::Void; test.locals.len()];
    let r = it.block(&test.body, &mut frame);
    let _ = it.out.flush();
    match r {
        Ok(_) => Ok(()),
        Err(Unwind::Panic(m)) => Err(m),
        Err(Unwind::OutOfSteps) => Err(step_limit_message(max_steps.unwrap())),
        Err(Unwind::Return(v)) => unreachable!("`?` in a test: {v:?}"),
    }
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

impl<'p> Interp<'p> {
    fn step(&mut self) -> R<()> {
        if let Some(n) = &mut self.steps_left {
            if *n == 0 {
                return Err(Unwind::OutOfSteps);
            }
            *n -= 1;
        }
        Ok(())
    }

    fn call(&mut self, fid: FuncId, args: Vec<Value>) -> R<Value> {
        let f = &self.program.funcs[fid];
        if self.depth >= MAX_CALL_DEPTH {
            return panic(format!("stack overflow: call depth exceeded {MAX_CALL_DEPTH} in `{}`", f.name));
        }
        self.depth += 1;
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
        self.depth -= 1;
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
            ExprKind::StructLit(_, fields) => Value::Struct(self.eval_all(fields, frame)?),
            ExprKind::ArrayLit(elems) => Value::Array(Rc::new(self.eval_all(elems, frame)?)),
            ExprKind::Variant { variant, fields, .. } => Value::Enum(*variant, self.eval_all(fields, frame)?),
            ExprKind::Index { base, index, site } => {
                let items = self.eval(base, frame)?;
                let i = self.eval(index, frame)?.as_i64();
                let Value::Array(items) = items else { unreachable!("index of {items:?}") };
                let len = items.len() as i64;
                if i < 0 || i >= len {
                    return panic(index_message(i, len, site));
                }
                items[i as usize].clone()
            }
            ExprKind::Interp(parts) => {
                let mut text = String::new();
                for p in parts {
                    let v = self.eval(p, frame)?;
                    text.push_str(&print_text(&v, &p.ty));
                }
                Value::Str(text.into())
            }
            ExprKind::StrConcat(l, r) => {
                let a = self.eval(l, frame)?;
                let b = self.eval(r, frame)?;
                Value::Str(format!("{}{}", a.as_str(), b.as_str()).into())
            }
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
        let x = self.eval(&args[0], frame)?;
        Ok(match b {
            Builtin::Print => {
                let mut line = print_text(&x, &args[0].ty);
                line.push('\n');
                self.out.write_all(line.as_bytes()).expect("stdout write failed");
                Value::Void
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
                Value::Array(items) => Value::I64(items.len() as i64),
                other => unreachable!("len of {other:?}"),
            },
        })
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
