//! Native backend: typed IR to an x86_64 object file with Cranelift, then to an executable.
//!
//! Every value is flattened into scalars: i64 -> I64, f64 -> F64, bool -> I8, structs -> their
//! fields in order, enums -> a tag then the fields of every variant (unused ones zero). str and
//! arrays are one pointer to a reference-counted object in the C runtime (anvil_rt.c).
//!
//! Ownership rules for heap pointers:
//! - `expr` returns owned values: each pointer carries one reference that the caller consumes
//!   (stores it in a local or in memory, passes it to a call, returns it) or releases.
//! - Every local owns its pointers. Locals start null, a store releases the old value, and all
//!   locals are released when the function returns.
//! - Arrays are copied on write: before a change, anvil_arr_unique gives the storage one owner.
//! - Array elements are stored as 8-byte slots, one per scalar, so an element of type T is
//!   `8 * flat_len(T)` bytes.

pub mod link;

use anvil_front::ir::*;
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{
    types, AbiParam, Block, BlockArg, FuncRef, InstBuilder, MemFlagsData, Signature, TrapCode, Type, UserFuncName,
    Value,
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

const I: Type = types::I64;

/// (name, params, returns) of every runtime function.
const RUNTIME: &[(&str, &[Type], &[Type])] = &[
    ("anvil_retain", &[I], &[]),
    ("anvil_release", &[I], &[]),
    ("anvil_str_concat", &[I, I], &[I]),
    ("anvil_str_cmp", &[I, I], &[I]),
    ("anvil_str_from_i64", &[I], &[I]),
    ("anvil_str_from_f64", &[types::F64], &[I]),
    ("anvil_str_from_bool", &[I], &[I]),
    ("anvil_print_line", &[I], &[]),
    ("anvil_panic", &[I], &[]),
    ("anvil_panic_index", &[I, I, I], &[]),
    ("anvil_arr_new", &[I, I], &[I]),
    ("anvil_arr_unique", &[I], &[I]),
    ("anvil_arr_push_slot", &[I], &[I]),
    ("anvil_arr_pop_slot", &[I], &[I]),
    ("anvil_exit", &[I], &[]),
    ("anvil_check_exit", &[I], &[]),
];

/// Object layout shared with anvil_rt.c.
const OBJ_LEN: i32 = 16;
const ARR_DATA: i32 = 40;
const SLOT: i64 = 8;

struct Backend {
    module: ObjectModule,
    funcs: Vec<ClFuncId>,
    runtime: HashMap<&'static str, ClFuncId>,
    strings: HashMap<String, DataId>,
    type_infos: HashMap<String, DataId>,
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
        type_infos: HashMap::new(),
    };

    for (name, params, rets) in RUNTIME {
        let mut sig = be.module.make_signature();
        sig.params.extend(params.iter().map(|t| AbiParam::new(*t)));
        sig.returns.extend(rets.iter().map(|t| AbiParam::new(*t)));
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
            listing.push_str(&format!("; fn {}\n{code}\n", f.name));
        }
    }
    if let Some(main) = program.main {
        be.c_main(program, main, &mut fbc)?;
    }
    let product = be.module.finish();
    Ok((product.emit().map_err(|e| e.to_string())?, listing))
}

/// One flattened scalar; `heap` marks a pointer to a reference-counted object.
#[derive(Clone, Copy, Debug)]
struct Scalar {
    ty: Type,
    heap: bool,
}

fn flat(program: &Program, ty: &Ty, out: &mut Vec<Scalar>) {
    match ty {
        Ty::Void => {}
        Ty::I64 => out.push(Scalar { ty: I, heap: false }),
        Ty::F64(_) => out.push(Scalar { ty: types::F64, heap: false }),
        Ty::Bool => out.push(Scalar { ty: types::I8, heap: false }),
        Ty::Str | Ty::Array(_) => out.push(Scalar { ty: I, heap: true }),
        Ty::Struct(id) => {
            for (_, t) in &program.structs[*id].fields {
                flat(program, t, out);
            }
        }
        Ty::Enum(id) => {
            out.push(Scalar { ty: I, heap: false });
            for v in &program.enums[*id].variants {
                for (_, t) in &v.fields {
                    flat(program, t, out);
                }
            }
        }
        Ty::Error => unreachable!("error type reached the backend"),
    }
}

fn layout(program: &Program, ty: &Ty) -> Vec<Scalar> {
    let mut v = Vec::new();
    flat(program, ty, &mut v);
    v
}

fn flat_len(program: &Program, ty: &Ty) -> usize {
    layout(program, ty).len()
}

/// Scalar range of field `idx` inside a flattened struct.
fn field_range(program: &Program, sid: StructId, idx: usize) -> (usize, usize) {
    let fields = &program.structs[sid].fields;
    let start: usize = fields[..idx].iter().map(|(_, t)| flat_len(program, t)).sum();
    (start, flat_len(program, &fields[idx].1))
}

/// Scalar range of `field` of `variant` inside a flattened enum (scalar 0 is the tag).
fn variant_field_range(program: &Program, eid: EnumId, variant: usize, field: usize) -> (usize, usize) {
    let variants = &program.enums[eid].variants;
    let before: usize =
        variants[..variant].iter().flat_map(|v| v.fields.iter()).map(|(_, t)| flat_len(program, t)).sum();
    let fields = &variants[variant].fields;
    let within: usize = fields[..field].iter().map(|(_, t)| flat_len(program, t)).sum();
    (1 + before + within, flat_len(program, &fields[field].1))
}

fn show(program: &Program, ty: &Ty) -> String {
    TyDisplay { ty, structs: &program.structs, enums: &program.enums }.to_string()
}

impl Backend {
    fn signature<'t>(&self, program: &Program, params: impl Iterator<Item = &'t Ty>, ret: &Ty) -> Signature {
        let mut sig = self.module.make_signature();
        for t in params {
            sig.params.extend(layout(program, t).iter().map(|s| AbiParam::new(s.ty)));
        }
        sig.returns.extend(layout(program, ret).iter().map(|s| AbiParam::new(s.ty)));
        sig
    }

    /// A static str object: refcount -1 (never freed), kind 0, length, bytes.
    fn string(&mut self, s: &str) -> DataId {
        if let Some(id) = self.strings.get(s) {
            return *id;
        }
        let name = format!("anvil_str_{}", self.strings.len());
        let id = self.module.declare_data(&name, Linkage::Local, false, false).expect("declare string");
        let mut bytes = Vec::new();
        bytes.extend((-1i64).to_le_bytes());
        bytes.extend(0i64.to_le_bytes());
        bytes.extend((s.len() as i64).to_le_bytes());
        bytes.extend(s.as_bytes());
        bytes.push(0);
        let mut desc = DataDescription::new();
        desc.set_align(8);
        desc.define(bytes.into_boxed_slice());
        self.module.define_data(id, &desc).expect("define string");
        self.strings.insert(s.to_string(), id);
        id
    }

    /// Element layout for the runtime: slot size in bytes, then the byte offsets of heap pointers.
    fn type_info(&mut self, program: &Program, elem: &Ty) -> DataId {
        let key = show(program, elem);
        if let Some(id) = self.type_infos.get(&key) {
            return *id;
        }
        let lay = layout(program, elem);
        let ptrs: Vec<i64> = lay.iter().enumerate().filter(|(_, s)| s.heap).map(|(k, _)| k as i64 * SLOT).collect();
        let mut bytes = Vec::new();
        bytes.extend((lay.len() as i64 * SLOT).to_le_bytes());
        bytes.extend((ptrs.len() as i64).to_le_bytes());
        for p in ptrs {
            bytes.extend(p.to_le_bytes());
        }
        let name = format!("anvil_ti_{}", self.type_infos.len());
        let id = self.module.declare_data(&name, Linkage::Local, false, false).expect("declare type info");
        let mut desc = DataDescription::new();
        desc.set_align(8);
        desc.define(bytes.into_boxed_slice());
        self.module.define_data(id, &desc).expect("define type info");
        self.type_infos.insert(key, id);
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
                _ => b.ins().iconst(I, 0),
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

/// Where a place lives: scalars `start..` of a local's variables, or memory inside an array.
enum Loc {
    Vars(LocalId, usize, Ty),
    Mem(Value, Ty),
}

struct Gen<'a, 'b> {
    be: &'a mut Backend,
    program: &'a Program,
    b: FunctionBuilder<'b>,
    /// Flattened variables for each local, and their layout.
    vars: Vec<Vec<Variable>>,
    layouts: Vec<Vec<Scalar>>,
    func_refs: HashMap<FuncId, FuncRef>,
    rt_refs: HashMap<&'static str, FuncRef>,
    /// (continue target, break target) for enclosing loops.
    loops: Vec<(Block, Block)>,
    exit: Block,
    local_types: Vec<Ty>,
    /// Flags for loads and stores of runtime objects: aligned and never trapping.
    trusted: MemFlagsData,
}

fn args(vals: &[Value]) -> Vec<BlockArg> {
    vals.iter().map(|v| BlockArg::Value(*v)).collect()
}

/// Whether evaluating `e` can change a local (only push and pop do).
fn mutates(e: &Expr) -> bool {
    let any = |es: &[Expr]| es.iter().any(mutates);
    match &e.kind {
        ExprKind::Push { .. } | ExprKind::Pop { .. } => true,
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_) | ExprKind::Local(_) => false,
        ExprKind::Field(x, _) | ExprKind::Not(x) | ExprKind::FNeg(x) | ExprKind::INeg { e: x, .. } => mutates(x),
        ExprKind::Try { inner, .. } => mutates(inner),
        ExprKind::Index { base, index, .. } => mutates(base) || mutates(index),
        ExprKind::IArith { l, r, .. }
        | ExprKind::FArith(_, l, r)
        | ExprKind::Cmp(_, l, r)
        | ExprKind::And(l, r)
        | ExprKind::Or(l, r)
        | ExprKind::StrConcat(l, r) => mutates(l) || mutates(r),
        ExprKind::Call(_, es)
        | ExprKind::StructLit(_, es)
        | ExprKind::ArrayLit(es)
        | ExprKind::Interp(es)
        | ExprKind::Builtin(_, es)
        | ExprKind::Variant { fields: es, .. } => any(es),
    }
}

impl<'a, 'b> Gen<'a, 'b> {
    fn new(be: &'a mut Backend, program: &'a Program, mut b: FunctionBuilder<'b>, locals: &[Local]) -> Gen<'a, 'b> {
        let mut vars = Vec::new();
        let mut layouts = Vec::new();
        for l in locals {
            let lay = layout(program, &l.ty);
            vars.push(lay.iter().map(|s| b.declare_var(s.ty)).collect());
            layouts.push(lay);
        }
        let exit = b.create_block();
        let local_types = locals.iter().map(|l| l.ty.clone()).collect();
        let trusted = MemFlagsData::trusted();
        Gen {
            be,
            program,
            b,
            vars,
            layouts,
            func_refs: HashMap::new(),
            rt_refs: HashMap::new(),
            loops: Vec::new(),
            exit,
            local_types,
            trusted,
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

    /// Calls a runtime function; returns its result, if any.
    fn call_rt(&mut self, name: &'static str, vals: &[Value]) -> Option<Value> {
        let f = self.rt(name);
        let call = self.b.ins().call(f, vals);
        self.b.inst_results(call).first().copied()
    }

    fn func_ref(&mut self, fid: FuncId) -> FuncRef {
        if let Some(r) = self.func_refs.get(&fid) {
            return *r;
        }
        let r = self.be.module.declare_func_in_func(self.be.funcs[fid], self.b.func);
        self.func_refs.insert(fid, r);
        r
    }

    fn layout(&self, ty: &Ty) -> Vec<Scalar> {
        layout(self.program, ty)
    }

    /// Pointer to a static str object.
    fn str_obj(&mut self, s: &str) -> Value {
        let data = self.be.string(s);
        let gv = self.be.module.declare_data_in_func(data, self.b.func);
        self.b.ins().symbol_value(I, gv)
    }

    fn retain(&mut self, lay: &[Scalar], vals: &[Value]) {
        for (s, v) in lay.iter().zip(vals) {
            if s.heap {
                self.call_rt("anvil_retain", &[*v]);
            }
        }
    }

    fn release(&mut self, lay: &[Scalar], vals: &[Value]) {
        for (s, v) in lay.iter().zip(vals) {
            if s.heap {
                self.call_rt("anvil_release", &[*v]);
            }
        }
    }

    fn zero(&mut self, s: Scalar) -> Value {
        if s.ty == types::F64 {
            self.b.ins().f64const(0.0)
        } else {
            self.b.ins().iconst(s.ty, 0)
        }
    }

    /// Branches to a cold panic block when `fail` is nonzero; continues in a fresh block otherwise.
    fn panic_if(&mut self, fail: Value, message: &str) {
        let panic = self.b.create_block();
        let cont = self.b.create_block();
        self.b.set_cold_block(panic);
        self.b.ins().brif(fail, panic, &[], cont, &[]);
        self.b.switch_to_block(panic);
        self.b.seal_block(panic);
        let m = self.str_obj(message);
        self.call_rt("anvil_panic", &[m]);
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
        // Every other local starts null, so releasing it before its first store is a no-op.
        for l in 0..self.vars.len() {
            if f.params.contains(&l) {
                continue;
            }
            for (k, s) in self.layouts[l].clone().into_iter().enumerate() {
                let z = self.zero(s);
                self.b.def_var(self.vars[l][k], z);
            }
        }
        for c in &f.requires {
            let ok = self.expr(&c.expr)[0];
            let fail = self.b.ins().icmp_imm_s(IntCC::Equal, ok, 0);
            self.panic_if(fail, &c.message);
        }
        for s in self.layout(&f.ret) {
            self.b.append_block_param(self.exit, s.ty);
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
        // `result` aliases the return values, which the caller now owns.
        for l in 0..self.vars.len() {
            if Some(l) == f.result_local {
                continue;
            }
            let vals = self.use_local(l);
            let lay = self.layouts[l].clone();
            self.release(&lay, &vals);
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

    fn use_local(&mut self, l: LocalId) -> Vec<Value> {
        self.vars[l].clone().into_iter().map(|v| self.b.use_var(v)).collect()
    }

    /// Stores owned `vals` into scalars `start..` of local `l`, releasing what was there.
    fn store_local(&mut self, l: LocalId, start: usize, vals: &[Value]) {
        for (k, x) in vals.iter().enumerate() {
            let var = self.vars[l][start + k];
            if self.layouts[l][start + k].heap {
                let old = self.b.use_var(var);
                self.call_rt("anvil_release", &[old]);
            }
            self.b.def_var(var, *x);
        }
    }

    fn load(&mut self, addr: Value, lay: &[Scalar]) -> Vec<Value> {
        lay.iter()
            .enumerate()
            .map(|(k, s)| self.b.ins().load(s.ty, self.trusted, addr, (k as i64 * SLOT) as i32))
            .collect()
    }

    fn store(&mut self, addr: Value, vals: &[Value]) {
        for (k, v) in vals.iter().enumerate() {
            self.b.ins().store(self.trusted, *v, addr, (k as i64 * SLOT) as i32);
        }
    }

    /// Address of element `index` of array `arr`, after a bounds check that panics at `site`.
    fn elem_addr(&mut self, arr: Value, index: Value, elem: &Ty, site: &str) -> Value {
        let len = self.b.ins().load(I, self.trusted, arr, OBJ_LEN);
        // Unsigned comparison also catches negative indices.
        let oob = self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
        let panic = self.b.create_block();
        let cont = self.b.create_block();
        self.b.set_cold_block(panic);
        self.b.ins().brif(oob, panic, &[], cont, &[]);
        self.b.switch_to_block(panic);
        self.b.seal_block(panic);
        let s = self.str_obj(site);
        self.call_rt("anvil_panic_index", &[index, len, s]);
        self.b.ins().trap(TrapCode::user(1).unwrap());
        self.b.switch_to_block(cont);
        self.b.seal_block(cont);
        let data = self.b.ins().load(I, self.trusted, arr, ARR_DATA);
        let size = flat_len(self.program, elem) as i64 * SLOT;
        let offset = self.b.ins().imul_imm_s(index, size);
        self.b.ins().iadd(data, offset)
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let(l, e) => {
                let vals = self.expr(e);
                self.store_local(*l, 0, &vals);
            }
            Stmt::Assign(l, path, e) => {
                let vals = self.expr(e);
                let indices = self.indices(path);
                let loc = self.locate(*l, path, &indices);
                self.write(loc, &vals);
            }
            Stmt::Expr(e) => {
                let vals = self.expr(e);
                let lay = self.layout(&e.ty);
                self.release(&lay, &vals);
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
                let limit = self.b.declare_var(I);
                self.b.def_var(limit, e0);
                self.counted_loop(s0, limit, body, |g, i| {
                    g.b.def_var(g.vars[*l][0], i);
                });
            }
            Stmt::ForEach { var, held, iter, body } => {
                let arr = self.expr(iter);
                self.store_local(*held, 0, &arr);
                let arr = arr[0];
                let len = self.b.ins().load(I, self.trusted, arr, OBJ_LEN);
                let limit = self.b.declare_var(I);
                self.b.def_var(limit, len);
                let zero = self.b.ins().iconst(I, 0);
                let Ty::Array(elem) = &iter.ty else { unreachable!("for over {:?}", iter.ty) };
                let elem = (**elem).clone();
                let held = *held;
                let var = *var;
                self.counted_loop(zero, limit, body, |g, i| {
                    // The snapshot in `held` is never written, so i < len is still in bounds.
                    let arr = g.b.use_var(g.vars[held][0]);
                    let data = g.b.ins().load(I, g.trusted, arr, ARR_DATA);
                    let size = flat_len(g.program, &elem) as i64 * SLOT;
                    let offset = g.b.ins().imul_imm_s(i, size);
                    let addr = g.b.ins().iadd(data, offset);
                    let lay = g.layout(&elem);
                    let vals = g.load(addr, &lay);
                    g.retain(&lay, &vals);
                    g.store_local(var, 0, &vals);
                });
            }
            Stmt::Match { held, scrutinee, arms } => {
                let vals = self.expr(scrutinee);
                self.store_local(*held, 0, &vals);
                let Ty::Enum(eid) = scrutinee.ty else { unreachable!("match on {:?}", scrutinee.ty) };
                let tag = self.b.use_var(self.vars[*held][0]);
                let merge = self.b.create_block();
                for (k, arm) in arms.iter().enumerate() {
                    let next = if k + 1 < arms.len() {
                        let this = self.b.create_block();
                        let next = self.b.create_block();
                        let hit = self.b.ins().icmp_imm_s(IntCC::Equal, tag, arm.variant as i64);
                        self.b.ins().brif(hit, this, &[], next, &[]);
                        self.b.switch_to_block(this);
                        self.b.seal_block(this);
                        Some(next)
                    } else {
                        None
                    };
                    for (field, local) in &arm.bindings {
                        let (start, n) = variant_field_range(self.program, eid, arm.variant, *field);
                        let vals = self.use_local(*held)[start..start + n].to_vec();
                        let lay = self.layouts[*held][start..start + n].to_vec();
                        self.retain(&lay, &vals);
                        self.store_local(*local, 0, &vals);
                    }
                    self.block(&arm.body);
                    self.b.ins().jump(merge, &[]);
                    if let Some(next) = next {
                        self.b.switch_to_block(next);
                        self.b.seal_block(next);
                    }
                }
                self.b.switch_to_block(merge);
                self.b.seal_block(merge);
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

    /// `for i in start..limit` with `bind` run at the top of each iteration.
    fn counted_loop(&mut self, start: Value, limit: Variable, body: &[Stmt], bind: impl FnOnce(&mut Self, Value)) {
        let counter = self.b.declare_var(I);
        self.b.def_var(counter, start);
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
        bind(self, i);
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

    /// The index values of a place, evaluated left to right.
    fn indices(&mut self, path: &[Step]) -> Vec<Value> {
        let mut out = Vec::new();
        for step in path {
            if let Step::Index { index, .. } = step {
                out.push(self.expr(index)[0]);
            }
        }
        out
    }

    /// Walks `path` from local `l` for writing: arrays on the way are made unique first.
    fn locate(&mut self, l: LocalId, path: &[Step], indices: &[Value]) -> Loc {
        let mut loc = Loc::Vars(l, 0, self.local_types[l].clone());
        let mut next_index = indices.iter();
        for step in path {
            loc = match step {
                Step::Field(i) => {
                    let ty = match &loc {
                        Loc::Vars(_, _, t) | Loc::Mem(_, t) => t.clone(),
                    };
                    let Ty::Struct(sid) = ty else { unreachable!("field of {ty:?}") };
                    let (start, _) = field_range(self.program, sid, *i);
                    let fty = self.program.structs[sid].fields[*i].1.clone();
                    match loc {
                        Loc::Vars(l, s, _) => Loc::Vars(l, s + start, fty),
                        Loc::Mem(addr, _) => Loc::Mem(self.b.ins().iadd_imm_s(addr, start as i64 * SLOT), fty),
                    }
                }
                Step::Index { site, .. } => {
                    let index = *next_index.next().unwrap();
                    let arr = self.unique_at(&loc);
                    let (Loc::Vars(_, _, Ty::Array(elem)) | Loc::Mem(_, Ty::Array(elem))) = &loc else {
                        unreachable!("index into a non-array place")
                    };
                    let elem = (**elem).clone();
                    let addr = self.elem_addr(arr, index, &elem, site);
                    Loc::Mem(addr, elem)
                }
            };
        }
        loc
    }

    /// Makes the array stored at `loc` unique (copying it if shared), stores it back, returns it.
    fn unique_at(&mut self, loc: &Loc) -> Value {
        match loc {
            Loc::Vars(l, start, _) => {
                let var = self.vars[*l][*start];
                let p = self.b.use_var(var);
                let q = self.call_rt("anvil_arr_unique", &[p]).unwrap();
                self.b.def_var(var, q);
                q
            }
            Loc::Mem(addr, _) => {
                let p = self.b.ins().load(I, self.trusted, *addr, 0);
                let q = self.call_rt("anvil_arr_unique", &[p]).unwrap();
                self.b.ins().store(self.trusted, q, *addr, 0);
                q
            }
        }
    }

    /// Stores owned `vals` at `loc`, releasing the old value.
    fn write(&mut self, loc: Loc, vals: &[Value]) {
        match loc {
            Loc::Vars(l, start, _) => self.store_local(l, start, vals),
            Loc::Mem(addr, ty) => {
                let lay = self.layout(&ty);
                let old = self.load(addr, &lay);
                self.release(&lay, &old);
                self.store(addr, vals);
            }
        }
    }

    /// Values of `e` without taking a reference when they can be read straight from a local.
    /// Returns (values, owned); owned values must be consumed or released by the caller.
    fn borrow(&mut self, e: &Expr) -> (Vec<Value>, bool) {
        match &e.kind {
            ExprKind::Local(l) => (self.use_local(*l), false),
            ExprKind::Field(base, idx) => {
                let (vals, owned) = self.borrow(base);
                let Ty::Struct(sid) = base.ty else { unreachable!() };
                let (s, n) = field_range(self.program, sid, *idx);
                if owned {
                    // Drop the other fields of the temporary struct.
                    let lay = self.layout(&base.ty);
                    let rest: Vec<(Scalar, Value)> = lay
                        .iter()
                        .zip(&vals)
                        .enumerate()
                        .filter(|(k, _)| *k < s || *k >= s + n)
                        .map(|(_, (sc, v))| (*sc, *v))
                        .collect();
                    let (rl, rv): (Vec<Scalar>, Vec<Value>) = rest.into_iter().unzip();
                    self.release(&rl, &rv);
                }
                (vals[s..s + n].to_vec(), owned)
            }
            _ => (self.expr(e), true),
        }
    }

    fn expr(&mut self, e: &Expr) -> Vec<Value> {
        match &e.kind {
            ExprKind::Int(v) => vec![self.b.ins().iconst(I, *v)],
            ExprKind::Float(v) => vec![self.b.ins().f64const(*v)],
            ExprKind::Bool(v) => vec![self.b.ins().iconst(types::I8, *v as i64)],
            // Static strings are never freed, so the pointer needs no reference.
            ExprKind::Str(s) => vec![self.str_obj(s)],
            ExprKind::Local(_) | ExprKind::Field(..) => {
                let (vals, owned) = self.borrow(e);
                if !owned {
                    let lay = self.layout(&e.ty);
                    self.retain(&lay, &vals);
                }
                vals
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
            ExprKind::ArrayLit(elems) => {
                let Ty::Array(elem) = &e.ty else { unreachable!() };
                let ti = self.be.type_info(self.program, elem);
                let gv = self.be.module.declare_data_in_func(ti, self.b.func);
                let ti = self.b.ins().symbol_value(I, gv);
                let cap = self.b.ins().iconst(I, elems.len() as i64);
                let arr = self.call_rt("anvil_arr_new", &[ti, cap]).unwrap();
                for x in elems {
                    let vals = self.expr(x);
                    let slot = self.call_rt("anvil_arr_push_slot", &[arr]).unwrap();
                    self.store(slot, &vals);
                }
                vec![arr]
            }
            ExprKind::Variant { enum_id, variant, fields } => {
                let mut given = Vec::new();
                for f in fields {
                    given.extend(self.expr(f));
                }
                self.enum_value(*enum_id, *variant, &given)
            }
            ExprKind::Index { base, index, site } => {
                // A borrowed array could be changed in place by a pop in the index expression.
                let (arr, owned) = if mutates(index) { (self.expr(base), true) } else { self.borrow(base) };
                let i = self.expr(index)[0];
                let lay = self.layout(&e.ty);
                let addr = self.elem_addr(arr[0], i, &e.ty, site);
                let vals = self.load(addr, &lay);
                self.retain(&lay, &vals);
                if owned {
                    self.call_rt("anvil_release", &[arr[0]]);
                }
                vals
            }
            ExprKind::Interp(parts) => {
                let mut acc = self.text(&parts[0]);
                for p in &parts[1..] {
                    let t = self.text(p);
                    let joined = self.call_rt("anvil_str_concat", &[acc, t]).unwrap();
                    self.call_rt("anvil_release", &[acc]);
                    self.call_rt("anvil_release", &[t]);
                    acc = joined;
                }
                vec![acc]
            }
            ExprKind::StrConcat(l, r) => {
                let a = self.expr(l)[0];
                let c = self.expr(r)[0];
                let joined = self.call_rt("anvil_str_concat", &[a, c]).unwrap();
                self.call_rt("anvil_release", &[a]);
                self.call_rt("anvil_release", &[c]);
                vec![joined]
            }
            ExprKind::Push { local, path, value } => {
                let vals = self.expr(value);
                let indices = self.indices(path);
                let loc = self.locate(*local, path, &indices);
                let arr = self.unique_at(&loc);
                let slot = self.call_rt("anvil_arr_push_slot", &[arr]).unwrap();
                self.store(slot, &vals);
                Vec::new()
            }
            ExprKind::Pop { local, path, message } => {
                let indices = self.indices(path);
                let loc = self.locate(*local, path, &indices);
                let arr = self.unique_at(&loc);
                let len = self.b.ins().load(I, self.trusted, arr, OBJ_LEN);
                let empty = self.b.ins().icmp_imm_s(IntCC::Equal, len, 0);
                self.panic_if(empty, message);
                // The element's references move from the array to the result.
                let slot = self.call_rt("anvil_arr_pop_slot", &[arr]).unwrap();
                let lay = self.layout(&e.ty);
                self.load(slot, &lay)
            }
            ExprKind::Try { held, inner, ret_enum } => {
                let vals = self.expr(inner);
                self.store_local(*held, 0, &vals);
                let Ty::Enum(eid) = inner.ty else { unreachable!() };
                let tag = self.b.use_var(self.vars[*held][0]);
                let ok_b = self.b.create_block();
                let fail_b = self.b.create_block();
                self.b.ins().brif(tag, fail_b, &[], ok_b, &[]);
                // Failure: return the same variant (None, or Err with the same error) of the return type.
                self.b.switch_to_block(fail_b);
                self.b.seal_block(fail_b);
                let failure = 1;
                let mut payload = Vec::new();
                for field in 0..self.program.enums[eid].variants[failure].fields.len() {
                    let (start, n) = variant_field_range(self.program, eid, failure, field);
                    let vals = self.use_local(*held)[start..start + n].to_vec();
                    let lay = self.layouts[*held][start..start + n].to_vec();
                    self.retain(&lay, &vals);
                    payload.extend(vals);
                }
                let ret = self.enum_value(*ret_enum, failure, &payload);
                self.b.ins().jump(self.exit, &args(&ret));
                self.b.switch_to_block(ok_b);
                self.b.seal_block(ok_b);
                let (start, n) = variant_field_range(self.program, eid, 0, 0);
                let vals = self.use_local(*held)[start..start + n].to_vec();
                let lay = self.layouts[*held][start..start + n].to_vec();
                self.retain(&lay, &vals);
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
                let icc = match op {
                    CmpOp::Eq => IntCC::Equal,
                    CmpOp::Ne => IntCC::NotEqual,
                    CmpOp::Lt => IntCC::SignedLessThan,
                    CmpOp::Le => IntCC::SignedLessThanOrEqual,
                    CmpOp::Gt => IntCC::SignedGreaterThan,
                    CmpOp::Ge => IntCC::SignedGreaterThanOrEqual,
                };
                let v = match l.ty {
                    Ty::F64(_) => {
                        let cc = match op {
                            CmpOp::Eq => FloatCC::Equal,
                            CmpOp::Ne => FloatCC::NotEqual,
                            CmpOp::Lt => FloatCC::LessThan,
                            CmpOp::Le => FloatCC::LessThanOrEqual,
                            CmpOp::Gt => FloatCC::GreaterThan,
                            CmpOp::Ge => FloatCC::GreaterThanOrEqual,
                        };
                        self.b.ins().fcmp(cc, a, c)
                    }
                    Ty::Str => {
                        let order = self.call_rt("anvil_str_cmp", &[a, c]).unwrap();
                        self.call_rt("anvil_release", &[a]);
                        self.call_rt("anvil_release", &[c]);
                        self.b.ins().icmp_imm_s(icc, order, 0)
                    }
                    _ => self.b.ins().icmp(icc, a, c),
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

    /// Flattened enum value: the tag, `given` for the variant's fields, zero everywhere else.
    fn enum_value(&mut self, eid: EnumId, variant: usize, given: &[Value]) -> Vec<Value> {
        let mut vals = vec![self.b.ins().iconst(I, variant as i64)];
        let variants = self.program.enums[eid].variants.clone();
        for (k, v) in variants.iter().enumerate() {
            for (_, t) in &v.fields {
                for s in self.layout(t) {
                    if k != variant {
                        let z = self.zero(s);
                        vals.push(z);
                    }
                }
            }
            if k == variant {
                vals.extend_from_slice(given);
            }
        }
        vals
    }

    /// An owned str holding the text of `e` (as print shows it).
    fn text(&mut self, e: &Expr) -> Value {
        let v = self.expr(e);
        match &e.ty {
            Ty::Str => v[0],
            Ty::I64 => self.call_rt("anvil_str_from_i64", &v).unwrap(),
            Ty::Bool => {
                let wide = self.b.ins().uextend(I, v[0]);
                self.call_rt("anvil_str_from_bool", &[wide]).unwrap()
            }
            Ty::F64(u) => {
                let digits = self.call_rt("anvil_str_from_f64", &v).unwrap();
                if u.is_dimensionless() {
                    return digits;
                }
                let unit = self.str_obj(&format!(" {u}"));
                let joined = self.call_rt("anvil_str_concat", &[digits, unit]).unwrap();
                self.call_rt("anvil_release", &[digits]);
                joined
            }
            t => unreachable!("text of {t:?}"),
        }
    }

    fn builtin(&mut self, bi: &Builtin, bargs: &[Expr]) -> Vec<Value> {
        match bi {
            Builtin::Print => {
                let t = self.text(&bargs[0]);
                self.call_rt("anvil_print_line", &[t]);
                self.call_rt("anvil_release", &[t]);
                return Vec::new();
            }
            Builtin::Len => {
                // str and arrays both keep their length at the same offset.
                let (vals, owned) = self.borrow(&bargs[0]);
                let len = self.b.ins().load(I, self.trusted, vals[0], OBJ_LEN);
                if owned {
                    self.call_rt("anvil_release", &[vals[0]]);
                }
                return vec![len];
            }
            _ => {}
        }
        let x = self.expr(&bargs[0])[0];
        match bi {
            Builtin::Print | Builtin::Len => unreachable!(),
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
