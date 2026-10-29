//! Native backend: typed IR to an x86_64 object file with Cranelift, then to an executable.
//!
//! Every value is flattened into scalars: i64 -> I64, f64 -> F64, bool -> I8, str -> (ptr, len),
//! structs -> their fields in order. Functions take and return flattened scalars, so structs never
//! touch memory and there is no layout to get wrong.

pub mod link;

use anvil_front::ir::*;
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{
    types, AbiParam, Block, BlockArg, FuncRef, InstBuilder, Signature, TrapCode, Type, UserFuncName, Value,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{default_libcall_names, DataDescription, DataId, FuncId as ClFuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::collections::HashMap;
use std::path::Path;

/// Compiles and links `program` into a native executable at `out`. With `asm`, also returns the
/// disassembly of every function, so generated code can be inspected from a terminal.
pub fn build_exe(program: &Program, out: &Path, work_dir: &Path, asm: bool) -> Result<String, String> {
    let (object, listing) = compile_object(program, asm)?;
    link::link_exe(&object, out, work_dir)?;
    Ok(listing)
}

const RUNTIME: &[(&str, &[Type])] = &[
    ("anvil_print_i64", &[types::I64]),
    ("anvil_print_f64", &[types::F64]),
    ("anvil_print_bool", &[types::I64]),
    ("anvil_print_str", &[types::I64, types::I64]),
    ("anvil_print_newline", &[]),
    ("anvil_panic", &[types::I64, types::I64]),
    ("anvil_exit", &[types::I64]),
    ("anvil_check_exit", &[types::I64]),
];

struct Backend {
    module: ObjectModule,
    funcs: Vec<ClFuncId>,
    runtime: HashMap<&'static str, ClFuncId>,
    strings: HashMap<String, DataId>,
}

pub fn compile_object(program: &Program, asm: bool) -> Result<(Vec<u8>, String), String> {
    let mut listing = String::new();
    let mut flags = settings::builder();
    let set = |f: &mut settings::Builder, k: &str, v: &str| f.set(k, v).map_err(|e| format!("cranelift flag {k}: {e}"));
    set(&mut flags, "opt_level", "speed")?;
    set(&mut flags, "enable_multi_ret_implicit_sret", "true")?;
    set(&mut flags, "enable_verifier", "true")?;
    let isa = cranelift_codegen::isa::lookup_by_name(link::TARGET)
        .map_err(|e| e.to_string())?
        .finish(settings::Flags::new(flags))
        .map_err(|e| e.to_string())?;
    let builder = ObjectBuilder::new(isa, "anvil_program", default_libcall_names()).map_err(|e| e.to_string())?;
    let mut be = Backend {
        module: ObjectModule::new(builder),
        funcs: Vec::new(),
        runtime: HashMap::new(),
        strings: HashMap::new(),
    };

    for (name, params) in RUNTIME {
        let mut sig = be.module.make_signature();
        sig.params.extend(params.iter().map(|t| AbiParam::new(*t)));
        let id = be.module.declare_function(name, Linkage::Import, &sig).map_err(|e| e.to_string())?;
        be.runtime.insert(name, id);
    }
    for f in &program.funcs {
        let sig = be.signature(program, f.params.iter().map(|p| &f.locals[*p].ty), &f.ret);
        let id =
            be.module.declare_function(&format!("anv_{}", f.name), Linkage::Local, &sig).map_err(|e| e.to_string())?;
        be.funcs.push(id);
    }

    let mut fbc = FunctionBuilderContext::new();
    for (i, f) in program.funcs.iter().enumerate() {
        let mut ctx = Context::new();
        ctx.set_disasm(asm);
        ctx.func.signature = be.signature(program, f.params.iter().map(|p| &f.locals[*p].ty), &f.ret);
        ctx.func.name = UserFuncName::user(0, be.funcs[i].as_u32());
        {
            let b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
            Gen::new(&mut be, program, b, &f.locals).function(f);
        }
        be.module
            .define_function(be.funcs[i], &mut ctx)
            .map_err(|e| format!("codegen of `{}` failed: {e:?}", f.name))?;
        if asm {
            let code = ctx.compiled_code().and_then(|c| c.vcode.clone()).unwrap_or_default();
            listing.push_str(&format!(
                "; fn {}
{code}
",
                f.name
            ));
        }
    }
    if let Some(main) = program.main {
        be.c_main(program, main, &mut fbc)?;
    }
    let product = be.module.finish();
    Ok((product.emit().map_err(|e| e.to_string())?, listing))
}

fn flat(program: &Program, ty: &Ty, out: &mut Vec<Type>) {
    match ty {
        Ty::Void => {}
        Ty::I64 => out.push(types::I64),
        Ty::F64(_) => out.push(types::F64),
        Ty::Bool => out.push(types::I8),
        Ty::Str => out.extend([types::I64, types::I64]),
        Ty::Struct(id) => {
            for (_, t) in &program.structs[*id].fields {
                flat(program, t, out);
            }
        }
        Ty::Error => unreachable!("error type reached the backend"),
    }
}

fn flat_len(program: &Program, ty: &Ty) -> usize {
    let mut v = Vec::new();
    flat(program, ty, &mut v);
    v.len()
}

/// Scalar range of field `idx` inside a flattened struct.
fn field_range(program: &Program, sid: StructId, idx: usize) -> (usize, usize) {
    let fields = &program.structs[sid].fields;
    let start: usize = fields[..idx].iter().map(|(_, t)| flat_len(program, t)).sum();
    (start, flat_len(program, &fields[idx].1))
}

impl Backend {
    fn signature<'t>(&self, program: &Program, params: impl Iterator<Item = &'t Ty>, ret: &Ty) -> Signature {
        let mut sig = self.module.make_signature();
        let mut ps = Vec::new();
        for t in params {
            flat(program, t, &mut ps);
        }
        let mut rs = Vec::new();
        flat(program, ret, &mut rs);
        sig.params.extend(ps.into_iter().map(AbiParam::new));
        sig.returns.extend(rs.into_iter().map(AbiParam::new));
        sig
    }

    fn string(&mut self, s: &str) -> DataId {
        if let Some(id) = self.strings.get(s) {
            return *id;
        }
        let name = format!("anvil_str_{}", self.strings.len());
        let id = self.module.declare_data(&name, Linkage::Local, false, false).expect("declare string");
        let mut desc = DataDescription::new();
        // At least one byte so empty strings still have an address.
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(0);
        desc.define(bytes.into_boxed_slice());
        self.module.define_data(id, &desc).expect("define string");
        self.strings.insert(s.to_string(), id);
        id
    }

    /// The C entry point: calls anv_main, validates the exit code, flushes and exits.
    fn c_main(&mut self, program: &Program, main: FuncId, fbc: &mut FunctionBuilderContext) -> Result<(), String> {
        let mut sig = self.module.make_signature();
        sig.returns.push(AbiParam::new(types::I32));
        let id = self.module.declare_function("main", Linkage::Export, &sig).map_err(|e| e.to_string())?;
        let mut ctx = Context::new();
        ctx.func.signature = sig;
        ctx.func.name = UserFuncName::user(1, id.as_u32());
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, fbc);
            let entry = b.create_block();
            b.switch_to_block(entry);
            b.seal_block(entry);
            let f = self.module.declare_func_in_func(self.funcs[main], b.func);
            let call = b.ins().call(f, &[]);
            let code = match program.funcs[main].ret {
                Ty::I64 => {
                    let code = b.inst_results(call)[0];
                    let check = self.module.declare_func_in_func(self.runtime["anvil_check_exit"], b.func);
                    b.ins().call(check, &[code]);
                    code
                }
                _ => b.ins().iconst(types::I64, 0),
            };
            let exit = self.module.declare_func_in_func(self.runtime["anvil_exit"], b.func);
            b.ins().call(exit, &[code]);
            let zero = b.ins().iconst(types::I32, 0);
            b.ins().return_(&[zero]);
            b.finalize(self.module.isa().frontend_config());
        }
        self.module.define_function(id, &mut ctx).map_err(|e| format!("codegen of main failed: {e:?}"))
    }
}

struct Gen<'a, 'b> {
    be: &'a mut Backend,
    program: &'a Program,
    b: FunctionBuilder<'b>,
    /// Flattened variables for each local.
    vars: Vec<Vec<Variable>>,
    func_refs: HashMap<FuncId, FuncRef>,
    rt_refs: HashMap<&'static str, FuncRef>,
    /// (continue target, break target) for enclosing loops.
    loops: Vec<(Block, Block)>,
    exit: Block,
    local_types: Vec<Ty>,
}

fn args(vals: &[Value]) -> Vec<BlockArg> {
    vals.iter().map(|v| BlockArg::Value(*v)).collect()
}

impl<'a, 'b> Gen<'a, 'b> {
    fn new(be: &'a mut Backend, program: &'a Program, mut b: FunctionBuilder<'b>, locals: &[Local]) -> Gen<'a, 'b> {
        let mut vars = Vec::new();
        for l in locals {
            let mut ts = Vec::new();
            flat(program, &l.ty, &mut ts);
            vars.push(ts.into_iter().map(|t| b.declare_var(t)).collect());
        }
        let exit = b.create_block();
        let local_types = locals.iter().map(|l| l.ty.clone()).collect();
        Gen {
            be,
            program,
            b,
            vars,
            func_refs: HashMap::new(),
            rt_refs: HashMap::new(),
            loops: Vec::new(),
            exit,
            local_types,
        }
    }

    fn rt(&mut self, name: &'static str) -> FuncRef {
        if let Some(r) = self.rt_refs.get(name) {
            return *r;
        }
        let r = self.be.module.declare_func_in_func(self.be.runtime[name], self.b.func);
        self.rt_refs.insert(name, r);
        r
    }

    fn func_ref(&mut self, fid: FuncId) -> FuncRef {
        if let Some(r) = self.func_refs.get(&fid) {
            return *r;
        }
        let r = self.be.module.declare_func_in_func(self.be.funcs[fid], self.b.func);
        self.func_refs.insert(fid, r);
        r
    }

    fn str_ptr(&mut self, s: &str) -> (Value, Value) {
        let data = self.be.string(s);
        let gv = self.be.module.declare_data_in_func(data, self.b.func);
        let ptr = self.b.ins().symbol_value(types::I64, gv);
        let len = self.b.ins().iconst(types::I64, s.len() as i64);
        (ptr, len)
    }

    /// Branches to a cold panic block when `fail` is nonzero; continues in a fresh block otherwise.
    fn panic_if(&mut self, fail: Value, message: &str) {
        let panic = self.b.create_block();
        let cont = self.b.create_block();
        self.b.set_cold_block(panic);
        self.b.ins().brif(fail, panic, &[], cont, &[]);
        self.b.switch_to_block(panic);
        self.b.seal_block(panic);
        let (p, l) = self.str_ptr(message);
        let f = self.rt("anvil_panic");
        self.b.ins().call(f, &[p, l]);
        self.b.ins().trap(TrapCode::user(1).unwrap());
        self.b.switch_to_block(cont);
        self.b.seal_block(cont);
    }

    fn function(mut self, f: &Func) {
        let entry = self.b.create_block();
        self.b.append_block_params_for_function_params(entry);
        self.b.switch_to_block(entry);
        self.b.seal_block(entry);
        let params: Vec<Value> = self.b.block_params(entry).to_vec();
        let mut i = 0;
        for p in &f.params {
            for v in self.vars[*p].clone() {
                self.b.def_var(v, params[i]);
                i += 1;
            }
        }
        for c in &f.requires {
            let ok = self.expr(&c.expr)[0];
            let fail = self.b.ins().icmp_imm_s(IntCC::Equal, ok, 0);
            self.panic_if(fail, &c.message);
        }
        let mut rets = Vec::new();
        flat(self.program, &f.ret, &mut rets);
        for t in &rets {
            self.b.append_block_param(self.exit, *t);
        }
        self.block(&f.body);
        if f.ret == Ty::Void {
            self.b.ins().jump(self.exit, &[]);
        } else {
            // The checker proved every path returns; this point is unreachable.
            self.b.ins().trap(TrapCode::user(2).unwrap());
        }
        self.b.switch_to_block(self.exit);
        let results: Vec<Value> = self.b.block_params(self.exit).to_vec();
        if let Some(r) = f.result_local {
            for (v, x) in self.vars[r].clone().into_iter().zip(&results) {
                self.b.def_var(v, *x);
            }
        }
        for c in &f.ensures {
            let ok = self.expr(&c.expr)[0];
            let fail = self.b.ins().icmp_imm_s(IntCC::Equal, ok, 0);
            self.panic_if(fail, &c.message);
        }
        self.b.ins().return_(&results);
        self.b.seal_all_blocks();
        let config = self.be.module.isa().frontend_config();
        self.b.finalize(config);
    }

    /// Continues codegen in a block nothing jumps to, after an unconditional transfer.
    fn dead_block(&mut self) {
        let dead = self.b.create_block();
        self.b.switch_to_block(dead);
    }

    fn block(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let(l, e) => {
                let vals = self.expr(e);
                for (v, x) in self.vars[*l].clone().into_iter().zip(vals) {
                    self.b.def_var(v, x);
                }
            }
            Stmt::Assign(l, path, e) => {
                let vals = self.expr(e);
                let (start, _) = self.path_range(*l, path);
                let vars = self.vars[*l].clone();
                for (k, x) in vals.into_iter().enumerate() {
                    self.b.def_var(vars[start + k], x);
                }
            }
            Stmt::Expr(e) => {
                self.expr(e);
            }
            Stmt::If(c, t, e) => {
                let cond = self.expr(c)[0];
                let then_b = self.b.create_block();
                let else_b = self.b.create_block();
                let merge = self.b.create_block();
                self.b.ins().brif(cond, then_b, &[], else_b, &[]);
                self.b.switch_to_block(then_b);
                self.b.seal_block(then_b);
                self.block(t);
                self.b.ins().jump(merge, &[]);
                self.b.switch_to_block(else_b);
                self.b.seal_block(else_b);
                self.block(e);
                self.b.ins().jump(merge, &[]);
                self.b.switch_to_block(merge);
                self.b.seal_block(merge);
            }
            Stmt::While(c, body) => {
                let header = self.b.create_block();
                let body_b = self.b.create_block();
                let exit = self.b.create_block();
                self.b.ins().jump(header, &[]);
                self.b.switch_to_block(header);
                let cond = self.expr(c)[0];
                self.b.ins().brif(cond, body_b, &[], exit, &[]);
                self.b.switch_to_block(body_b);
                self.b.seal_block(body_b);
                self.loops.push((header, exit));
                self.block(body);
                self.loops.pop();
                self.b.ins().jump(header, &[]);
                self.b.seal_block(header);
                self.b.switch_to_block(exit);
                self.b.seal_block(exit);
            }
            Stmt::For(l, start, end, body) => {
                let s0 = self.expr(start)[0];
                let e0 = self.expr(end)[0];
                let counter = self.b.declare_var(types::I64);
                let limit = self.b.declare_var(types::I64);
                self.b.def_var(counter, s0);
                self.b.def_var(limit, e0);
                let header = self.b.create_block();
                let body_b = self.b.create_block();
                let step = self.b.create_block();
                let exit = self.b.create_block();
                self.b.ins().jump(header, &[]);
                self.b.switch_to_block(header);
                let i = self.b.use_var(counter);
                let lim = self.b.use_var(limit);
                let more = self.b.ins().icmp(IntCC::SignedLessThan, i, lim);
                self.b.ins().brif(more, body_b, &[], exit, &[]);
                self.b.switch_to_block(body_b);
                self.b.seal_block(body_b);
                let i = self.b.use_var(counter);
                self.b.def_var(self.vars[*l][0], i);
                self.loops.push((step, exit));
                self.block(body);
                self.loops.pop();
                self.b.ins().jump(step, &[]);
                self.b.switch_to_block(step);
                self.b.seal_block(step);
                // i < limit <= i64::MAX, so this cannot overflow.
                let i = self.b.use_var(counter);
                let next = self.b.ins().iadd_imm_s(i, 1);
                self.b.def_var(counter, next);
                self.b.ins().jump(header, &[]);
                self.b.seal_block(header);
                self.b.switch_to_block(exit);
                self.b.seal_block(exit);
            }
            Stmt::Return(v) => {
                let vals = match v {
                    Some(e) => self.expr(e),
                    None => Vec::new(),
                };
                self.b.ins().jump(self.exit, &args(&vals));
                self.dead_block();
            }
            Stmt::Break => {
                let (_, brk) = *self.loops.last().unwrap();
                self.b.ins().jump(brk, &[]);
                self.dead_block();
            }
            Stmt::Continue => {
                let (cont, _) = *self.loops.last().unwrap();
                self.b.ins().jump(cont, &[]);
                self.dead_block();
            }
        }
    }

    /// Scalar range inside local `l` addressed by a field path.
    fn path_range(&self, l: LocalId, path: &[usize]) -> (usize, usize) {
        let mut ty = self.local_ty(l);
        let mut start = 0;
        let mut len = self.vars[l].len();
        for idx in path {
            let Ty::Struct(sid) = ty else { unreachable!("field path into non-struct") };
            let (s, n) = field_range(self.program, sid, *idx);
            start += s;
            len = n;
            ty = self.program.structs[sid].fields[*idx].1.clone();
        }
        (start, len)
    }

    fn local_ty(&self, l: LocalId) -> Ty {
        self.local_types[l].clone()
    }

    fn expr(&mut self, e: &Expr) -> Vec<Value> {
        match &e.kind {
            ExprKind::Int(v) => vec![self.b.ins().iconst(types::I64, *v)],
            ExprKind::Float(v) => vec![self.b.ins().f64const(*v)],
            ExprKind::Bool(v) => vec![self.b.ins().iconst(types::I8, *v as i64)],
            ExprKind::Str(s) => {
                let (p, l) = self.str_ptr(s);
                vec![p, l]
            }
            ExprKind::Local(l) => self.vars[*l].clone().into_iter().map(|v| self.b.use_var(v)).collect(),
            ExprKind::Field(base, idx) => {
                let vals = self.expr(base);
                let Ty::Struct(sid) = base.ty else { unreachable!() };
                let (s, n) = field_range(self.program, sid, *idx);
                vals[s..s + n].to_vec()
            }
            ExprKind::Call(fid, call_args) => {
                let mut vals = Vec::new();
                for a in call_args {
                    vals.extend(self.expr(a));
                }
                let f = self.func_ref(*fid);
                let call = self.b.ins().call(f, &vals);
                self.b.inst_results(call).to_vec()
            }
            ExprKind::StructLit(_, fields) => {
                let mut vals = Vec::new();
                for f in fields {
                    vals.extend(self.expr(f));
                }
                vals
            }
            ExprKind::IArith { op, l, r, overflow, div_zero } => {
                let a = self.expr(l)[0];
                let c = self.expr(r)[0];
                let v = match op {
                    IOp::Add | IOp::Sub | IOp::Mul => {
                        let (v, of) = match op {
                            IOp::Add => self.b.ins().sadd_overflow(a, c),
                            IOp::Sub => self.b.ins().ssub_overflow(a, c),
                            _ => self.b.ins().smul_overflow(a, c),
                        };
                        self.panic_if(of, overflow);
                        v
                    }
                    IOp::Div | IOp::Rem => {
                        let zero = self.b.ins().icmp_imm_s(IntCC::Equal, c, 0);
                        self.panic_if(zero, div_zero);
                        let is_min = self.b.ins().icmp_imm_s(IntCC::Equal, a, i64::MIN);
                        let is_m1 = self.b.ins().icmp_imm_s(IntCC::Equal, c, -1);
                        let both = self.b.ins().band(is_min, is_m1);
                        self.panic_if(both, overflow);
                        if *op == IOp::Div {
                            self.b.ins().sdiv(a, c)
                        } else {
                            self.b.ins().srem(a, c)
                        }
                    }
                };
                vec![v]
            }
            ExprKind::FArith(op, l, r) => {
                let a = self.expr(l)[0];
                let c = self.expr(r)[0];
                vec![match op {
                    FOp::Add => self.b.ins().fadd(a, c),
                    FOp::Sub => self.b.ins().fsub(a, c),
                    FOp::Mul => self.b.ins().fmul(a, c),
                    FOp::Div => self.b.ins().fdiv(a, c),
                }]
            }
            ExprKind::Cmp(op, l, r) => {
                let a = self.expr(l)[0];
                let c = self.expr(r)[0];
                let v = if matches!(l.ty, Ty::F64(_)) {
                    let cc = match op {
                        CmpOp::Eq => FloatCC::Equal,
                        CmpOp::Ne => FloatCC::NotEqual,
                        CmpOp::Lt => FloatCC::LessThan,
                        CmpOp::Le => FloatCC::LessThanOrEqual,
                        CmpOp::Gt => FloatCC::GreaterThan,
                        CmpOp::Ge => FloatCC::GreaterThanOrEqual,
                    };
                    self.b.ins().fcmp(cc, a, c)
                } else {
                    let cc = match op {
                        CmpOp::Eq => IntCC::Equal,
                        CmpOp::Ne => IntCC::NotEqual,
                        CmpOp::Lt => IntCC::SignedLessThan,
                        CmpOp::Le => IntCC::SignedLessThanOrEqual,
                        CmpOp::Gt => IntCC::SignedGreaterThan,
                        CmpOp::Ge => IntCC::SignedGreaterThanOrEqual,
                    };
                    self.b.ins().icmp(cc, a, c)
                };
                vec![v]
            }
            ExprKind::And(l, r) | ExprKind::Or(l, r) => {
                let is_and = matches!(e.kind, ExprKind::And(..));
                let a = self.expr(l)[0];
                let rhs = self.b.create_block();
                let merge = self.b.create_block();
                let res = self.b.append_block_param(merge, types::I8);
                if is_and {
                    self.b.ins().brif(a, rhs, &[], merge, &args(&[a]));
                } else {
                    self.b.ins().brif(a, merge, &args(&[a]), rhs, &[]);
                }
                self.b.switch_to_block(rhs);
                self.b.seal_block(rhs);
                let c = self.expr(r)[0];
                self.b.ins().jump(merge, &args(&[c]));
                self.b.switch_to_block(merge);
                self.b.seal_block(merge);
                vec![res]
            }
            ExprKind::Not(x) => {
                let v = self.expr(x)[0];
                vec![self.b.ins().bxor_imm_u(v, 1)]
            }
            ExprKind::INeg { e: x, overflow } => {
                let v = self.expr(x)[0];
                let is_min = self.b.ins().icmp_imm_s(IntCC::Equal, v, i64::MIN);
                self.panic_if(is_min, overflow);
                vec![self.b.ins().ineg(v)]
            }
            ExprKind::FNeg(x) => {
                let v = self.expr(x)[0];
                vec![self.b.ins().fneg(v)]
            }
            ExprKind::Builtin(bi, bargs) => self.builtin(bi, bargs),
        }
    }

    fn builtin(&mut self, bi: &Builtin, bargs: &[Expr]) -> Vec<Value> {
        if let Builtin::Print = bi {
            for a in bargs {
                let vals = self.expr(a);
                match &a.ty {
                    Ty::I64 => {
                        let f = self.rt("anvil_print_i64");
                        self.b.ins().call(f, &vals);
                    }
                    Ty::F64(u) => {
                        let f = self.rt("anvil_print_f64");
                        self.b.ins().call(f, &vals);
                        if !u.is_dimensionless() {
                            let (p, l) = self.str_ptr(&format!(" {u}"));
                            let f = self.rt("anvil_print_str");
                            self.b.ins().call(f, &[p, l]);
                        }
                    }
                    Ty::Bool => {
                        let wide = self.b.ins().uextend(types::I64, vals[0]);
                        let f = self.rt("anvil_print_bool");
                        self.b.ins().call(f, &[wide]);
                    }
                    Ty::Str => {
                        let f = self.rt("anvil_print_str");
                        self.b.ins().call(f, &vals);
                    }
                    t => unreachable!("print of {t:?}"),
                }
            }
            let f = self.rt("anvil_print_newline");
            self.b.ins().call(f, &[]);
            return Vec::new();
        }
        let x = self.expr(&bargs[0])[0];
        match bi {
            Builtin::Print => unreachable!(),
            Builtin::Assert { message } => {
                let fail = self.b.ins().icmp_imm_s(IntCC::Equal, x, 0);
                self.panic_if(fail, message);
                Vec::new()
            }
            Builtin::Sqrt => vec![self.b.ins().sqrt(x)],
            Builtin::AbsI64 { message } => {
                let is_min = self.b.ins().icmp_imm_s(IntCC::Equal, x, i64::MIN);
                self.panic_if(is_min, message);
                vec![self.b.ins().iabs(x)]
            }
            Builtin::AbsF64 => vec![self.b.ins().fabs(x)],
            Builtin::ToF64 => vec![self.b.ins().fcvt_from_sint(types::F64, x)],
            Builtin::ToI64 { message } => {
                // Ordered comparisons are false for NaN, so NaN fails too.
                let lo = self.b.ins().f64const(TO_I64_MIN);
                let hi = self.b.ins().f64const(TO_I64_MAX_EXCLUSIVE);
                let ge = self.b.ins().fcmp(FloatCC::GreaterThanOrEqual, x, lo);
                let lt = self.b.ins().fcmp(FloatCC::LessThan, x, hi);
                let ok = self.b.ins().band(ge, lt);
                let fail = self.b.ins().icmp_imm_s(IntCC::Equal, ok, 0);
                self.panic_if(fail, message);
                vec![self.b.ins().fcvt_to_sint(types::I64, x)]
            }
        }
    }
}
