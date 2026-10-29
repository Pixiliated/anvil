//! Reference interpreter over the typed IR. It defines what a program means; the native backend
//! is tested against it (same stdout, same panic message, same exit code).

use anvil_front::ir::*;
use std::io::Write;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Void,
    I64(i64),
    F64(f64),
    Bool(bool),
    Str(std::rc::Rc<str>),
    Struct(Vec<Value>),
}

/// A program-level failure: the text after "panic: ".
#[derive(Debug, PartialEq)]
pub struct Panic(pub String);

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
}

/// How a run ended. Exit codes match the native executable: main's value, or 101 on panic.
pub enum Outcome {
    Exit(i32),
    Panicked(String),
}

/// Runs main. Needs a large stack for deep recursion; see `run_on_big_stack`.
pub fn run_main(program: &Program, out: &mut dyn Write) -> Outcome {
    let main = program.main.expect("run_main needs a main function");
    let mut it = Interp { program, out, depth: 0 };
    let result = it.call(main, Vec::new());
    let _ = it.out.flush();
    match result {
        Ok(Value::Void) => Outcome::Exit(0),
        Ok(Value::I64(code)) if (0..=255).contains(&code) => Outcome::Exit(code as i32),
        Ok(Value::I64(code)) => Outcome::Panicked(main_exit_message(code)),
        Ok(other) => unreachable!("main returned {other:?}"),
        Err(Panic(m)) => Outcome::Panicked(m),
    }
}

/// Runs one test body. Ok(()) if it finished, Err(message) if it panicked.
pub fn run_test(program: &Program, index: usize, out: &mut dyn Write) -> Result<(), String> {
    let test = &program.tests[index];
    let mut it = Interp { program, out, depth: 0 };
    let mut frame = vec![Value::Void; test.locals.len()];
    let r = it.block(&test.body, &mut frame);
    let _ = it.out.flush();
    r.map(|_| ()).map_err(|Panic(m)| m)
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

/// Text print() writes for one value; floats with a unit get " unit" appended.
pub fn print_text(v: &Value, ty: &Ty) -> String {
    match (v, ty) {
        (Value::I64(x), _) => x.to_string(),
        (Value::F64(x), Ty::F64(u)) if !u.is_dimensionless() => format!("{} {u}", format_f64(*x)),
        (Value::F64(x), _) => format_f64(*x),
        (Value::Bool(b), _) => b.to_string(),
        (Value::Str(s), _) => s.to_string(),
        (v, _) => unreachable!("print of {v:?}"),
    }
}

impl<'p> Interp<'p> {
    fn call(&mut self, fid: FuncId, args: Vec<Value>) -> Result<Value, Panic> {
        let f = &self.program.funcs[fid];
        if self.depth >= MAX_CALL_DEPTH {
            return Err(Panic(format!("stack overflow: call depth exceeded {MAX_CALL_DEPTH} in `{}`", f.name)));
        }
        self.depth += 1;
        let mut frame = vec![Value::Void; f.locals.len()];
        for (p, a) in f.params.iter().zip(args) {
            frame[*p] = a;
        }
        for c in &f.requires {
            if !self.eval(&c.expr, &mut frame)?.as_bool() {
                return Err(Panic(c.message.clone()));
            }
        }
        let ret = match self.block(&f.body, &mut frame)? {
            Flow::Return(v) => v,
            _ => Value::Void,
        };
        if let Some(r) = f.result_local {
            frame[r] = ret.clone();
        }
        for c in &f.ensures {
            if !self.eval(&c.expr, &mut frame)?.as_bool() {
                return Err(Panic(c.message.clone()));
            }
        }
        self.depth -= 1;
        Ok(ret)
    }

    fn block(&mut self, stmts: &[Stmt], frame: &mut Vec<Value>) -> Result<Flow, Panic> {
        for s in stmts {
            match self.stmt(s, frame)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    fn stmt(&mut self, s: &Stmt, frame: &mut Vec<Value>) -> Result<Flow, Panic> {
        match s {
            Stmt::Let(l, e) => {
                frame[*l] = self.eval(e, frame)?;
            }
            Stmt::Assign(l, path, e) => {
                let v = self.eval(e, frame)?;
                let mut slot = &mut frame[*l];
                for i in path {
                    let Value::Struct(fields) = slot else { unreachable!("field path into non-struct") };
                    slot = &mut fields[*i];
                }
                *slot = v;
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
                    match self.block(body, frame)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Normal | Flow::Continue => {}
                    }
                }
            }
            Stmt::For(l, start, end, body) => {
                let s = self.eval(start, frame)?.as_i64();
                let e = self.eval(end, frame)?.as_i64();
                let mut i = s;
                while i < e {
                    frame[*l] = Value::I64(i);
                    match self.block(body, frame)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Normal | Flow::Continue => {}
                    }
                    i += 1;
                }
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

    fn eval(&mut self, e: &Expr, frame: &mut Vec<Value>) -> Result<Value, Panic> {
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
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval(a, frame)?);
                }
                self.call(*fid, vals)?
            }
            ExprKind::StructLit(_, fields) => {
                let mut vals = Vec::with_capacity(fields.len());
                for f in fields {
                    vals.push(self.eval(f, frame)?);
                }
                Value::Struct(vals)
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
                            return Err(Panic(div_zero.clone()));
                        }
                        if *op == IOp::Div {
                            a.checked_div(b)
                        } else {
                            a.checked_rem(b)
                        }
                    }
                };
                Value::I64(v.ok_or_else(|| Panic(overflow.clone()))?)
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
                    _ => unreachable!("compare {a:?} {b:?}"),
                })
            }
            ExprKind::And(l, r) => Value::Bool(self.eval(l, frame)?.as_bool() && self.eval(r, frame)?.as_bool()),
            ExprKind::Or(l, r) => Value::Bool(self.eval(l, frame)?.as_bool() || self.eval(r, frame)?.as_bool()),
            ExprKind::Not(x) => Value::Bool(!self.eval(x, frame)?.as_bool()),
            ExprKind::INeg { e, overflow } => {
                Value::I64(self.eval(e, frame)?.as_i64().checked_neg().ok_or_else(|| Panic(overflow.clone()))?)
            }
            ExprKind::FNeg(x) => Value::F64(-self.eval(x, frame)?.as_f64()),
            ExprKind::Builtin(b, args) => self.builtin(b, args, frame)?,
        })
    }

    fn builtin(&mut self, b: &Builtin, args: &[Expr], frame: &mut Vec<Value>) -> Result<Value, Panic> {
        if let Builtin::Print = b {
            let mut line = String::new();
            for a in args {
                let v = self.eval(a, frame)?;
                line.push_str(&print_text(&v, &a.ty));
            }
            line.push('\n');
            self.out.write_all(line.as_bytes()).expect("stdout write failed");
            return Ok(Value::Void);
        }
        let x = self.eval(&args[0], frame)?;
        Ok(match b {
            Builtin::Print => unreachable!(),
            Builtin::Assert { message } => {
                if !x.as_bool() {
                    return Err(Panic(message.clone()));
                }
                Value::Void
            }
            Builtin::Sqrt => Value::F64(x.as_f64().sqrt()),
            Builtin::AbsI64 { message } => Value::I64(x.as_i64().checked_abs().ok_or_else(|| Panic(message.clone()))?),
            Builtin::AbsF64 => Value::F64(x.as_f64().abs()),
            Builtin::ToF64 => Value::F64(x.as_i64() as f64),
            Builtin::ToI64 { message } => {
                let v = x.as_f64();
                if !(TO_I64_MIN..TO_I64_MAX_EXCLUSIVE).contains(&v) {
                    return Err(Panic(message.clone()));
                }
                Value::I64(v as i64)
            }
        })
    }
}

fn cmp<T: PartialOrd>(op: CmpOp, a: T, b: T) -> bool {
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
}
