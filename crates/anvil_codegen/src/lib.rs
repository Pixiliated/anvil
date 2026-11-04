//! Native backend: typed IR to an x86_64 object file with Cranelift, then to an executable.
//!
//! Every value is flattened into scalars: i64 -> I64, f64 -> F64, bool -> I8, structs -> their
//! fields in order, enums -> a tag then the fields of every variant (unused ones zero). str,
//! arrays and maps are one pointer to a reference-counted object in the C runtime (anvil_rt.c);
//! a map is an array of (key, value) elements sorted by key.
//!
//! `==` on arrays, structs, enums and maps is structural: structs and enums compare inline (the
//! unused fields of an enum are zero on both sides), arrays and maps call a generated helper.
//!
//! Ownership rules for heap pointers:
//! - `expr` returns owned values: each pointer carries one reference that the caller consumes
//!   (stores it in a local or in memory, passes it to a call, returns it) or releases.
//! - Every local owns its pointers. Locals start null, a store releases the old value, and all
//!   locals are released when the function returns.
//! - Arrays are copied on write: before a change, anvil_arr_unique gives the storage one owner.
//! - Array elements are stored as 8-byte slots, one per scalar, so an element of type T is
//!   `8 * flat_len(T)` bytes.

pub mod ffi;
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
    let objects = link::compile_c(&program.c_files, work_dir)?;
    link::link_exe(&object, &objects, out, work_dir)?;
    Ok(listing)
}

/// Builds the program's plain tests (property tests run in the interpreter) into one
/// executable: `exe N` runs test N, its index in `program.tests`, and exits 0 when it passes or
/// panics like any program. Returns the indices of the tests it contains.
pub fn build_test_exe(program: &Program, out: &Path, work_dir: &Path) -> Result<Vec<usize>, String> {
    let mut p = program.clone();
    p.main = None;
    let mut entries = Vec::new();
    for (i, t) in program.tests.iter().enumerate().filter(|(_, t)| t.inputs.is_empty()) {
        p.funcs.push(Func {
            name: format!("test{i}"),
            span: Default::default(),
            allows: Vec::new(),
            module: 0,
            instance: false,
            params: Vec::new(),
            ret: Ty::Void,
            effects: t.effects,
            external: None,
            requires: Vec::new(),
            ensures: Vec::new(),
            steps: None,
            allocs: None,
            result_local: None,
            locals: t.locals.clone(),
            body: t.body.clone(),
            captures: None,
        });
        entries.push((i, p.funcs.len() - 1));
    }
    let (object, _) = compile(&p, false, Entry::Tests(&entries))?;
    let objects = link::compile_c(&program.c_files, work_dir)?;
    link::link_exe(&object, &objects, out, work_dir)?;
    Ok(entries.iter().map(|(i, _)| *i).collect())
}

/// What the executable's C `main` runs.
enum Entry<'e> {
    /// The program's main, if it has one.
    Main,
    /// (test index, FuncId of its body), chosen by the first argument.
    Tests(&'e [(usize, FuncId)]),
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
    ("anvil_print_line", &[I, I], &[]),
    ("anvil_print_i64", &[I, I], &[]),
    ("anvil_print_bool", &[I, I], &[]),
    ("anvil_print_f64", &[types::F64, I, I], &[]),
    ("anvil_exit_now", &[I, I], &[]),
    ("anvil_env", &[I], &[I]),
    ("anvil_list_dir", &[I, I], &[I]),
    ("anvil_clock_ns", &[], &[I]),
    ("anvil_random_below", &[I, I], &[I]),
    ("anvil_random_f64", &[], &[types::F64]),
    ("anvil_random_seed", &[I], &[]),
    ("anvil_args", &[I], &[I]),
    ("anvil_read_line", &[I], &[I]),
    ("anvil_read_file", &[I], &[I]),
    ("anvil_write_file", &[I, I], &[I]),
    ("anvil_file_error", &[], &[I]),
    ("anvil_str_trim", &[I], &[I]),
    ("anvil_str_parse_i64", &[I], &[I]),
    ("anvil_str_parse_f64", &[I], &[types::F64]),
    ("anvil_parse_ok", &[], &[I]),
    ("anvil_fn_new", &[I, I], &[I]),
    ("anvil_read_bytes", &[I], &[I]),
    ("anvil_bytes_of", &[I, I], &[I]),
    ("anvil_bytes_to_str", &[I], &[I]),
    ("anvil_bytes_hex", &[I], &[I]),
    ("anvil_bytes_to_array", &[I, I], &[I]),
    ("anvil_bytes_at", &[I, I, I], &[I]),
    ("anvil_bytes_slice", &[I, I, I, I], &[I]),
    ("anvil_check_shift", &[I, I], &[I]),
    ("anvil_str_replace", &[I, I, I, I], &[I]),
    ("anvil_str_case", &[I, I], &[I]),
    ("anvil_str_chars", &[I, I], &[I]),
    ("anvil_arr_join", &[I, I], &[I]),
    ("anvil_f64_fixed", &[types::F64, I, I], &[I]),
    ("anvil_arr_reversed", &[I], &[I]),
    ("anvil_arr_sorted", &[I, I, I], &[I]),
    ("anvil_parallel_map", &[I, I, I, I], &[I]),
    ("anvil_parallel_at", &[I], &[I]),
    ("anvil_panic", &[I], &[]),
    ("anvil_panic_index", &[I, I, I], &[]),
    ("anvil_arr_new", &[I, I], &[I]),
    ("anvil_arr_unique", &[I], &[I]),
    ("anvil_arr_push_slot", &[I], &[I]),
    ("anvil_arr_pop_slot", &[I], &[I]),
    ("anvil_arr_insert_slot", &[I, I], &[I]),
    ("anvil_arr_remove_slot", &[I, I], &[]),
    ("anvil_arr_slice", &[I, I, I, I], &[I]),
    ("anvil_str_slice", &[I, I, I, I], &[I]),
    ("anvil_str_find", &[I, I], &[I]),
    ("anvil_str_starts", &[I, I], &[I]),
    ("anvil_str_ends", &[I, I], &[I]),
    ("anvil_str_split", &[I, I, I, I], &[I]),
    ("anvil_str_eq", &[I, I], &[I]),
    ("anvil_map_find_i64", &[I, I], &[I]),
    ("anvil_map_find_str", &[I, I], &[I]),
    ("anvil_test_index", &[], &[I]),
    ("anvil_no_test", &[I], &[]),
    ("anvil_exit", &[I], &[]),
    ("anvil_check_exit", &[I], &[]),
];

/// The function the C runtime's `main` runs: the program's main, or a test chosen by argv.
const ENTRY: &str = "anvil_program";

/// Object layout shared with anvil_rt.c.
const OBJ_LEN: i32 = 16;
const ARR_DATA: i32 = 40;
const SLOT: i64 = 8;
/// A function value: rc, kind, the layout of its captures (at 16), its code (at 24), then the
/// captured values. The code takes the object itself first, then the call's arguments.
const FN_CODE: i32 = 24;
const FN_ENV: i64 = 32;
const KIND_FN: i64 = 2;

struct Backend {
    module: ObjectModule,
    funcs: Vec<ClFuncId>,
    runtime: HashMap<&'static str, ClFuncId>,
    strings: HashMap<String, DataId>,
    type_infos: HashMap<String, DataId>,
    /// `==` helpers for arrays and maps by type name, and those declared but not yet generated.
    eq_helpers: HashMap<String, ClFuncId>,
    pending_eq: Vec<(ClFuncId, Ty)>,
    /// A declared function's value code: a thunk taking (closure, args) that calls it; those
    /// declared but not yet generated. Static function objects by their code.
    thunks: HashMap<FuncId, ClFuncId>,
    pending_thunks: Vec<(FuncId, ClFuncId)>,
    fn_objects: HashMap<ClFuncId, DataId>,
    /// parallel_map's workers by (element, result) type, and those declared but not yet generated.
    workers: HashMap<String, ClFuncId>,
    pending_workers: Vec<(ClFuncId, Ty, Ty)>,
}

pub fn compile_object(program: &Program, asm: bool) -> Result<(Vec<u8>, String), String> {
    compile(program, asm, Entry::Main)
}

fn compile(program: &Program, asm: bool, entry: Entry) -> Result<(Vec<u8>, String), String> {
    let mut listing = String::new();
    let mut flags = settings::builder();
    let set = |f: &mut settings::Builder, k: &str, v: &str| f.set(k, v).map_err(|e| format!("cranelift flag {k}: {e}"));
    set(&mut flags, "opt_level", "speed")?;
    set(&mut flags, "enable_multi_ret_implicit_sret", "true")?;
    set(&mut flags, "enable_verifier", "true")?;
    // ELF and Mach-O executables are position independent (PIE); Windows links at a fixed base.
    if !cfg!(windows) {
        set(&mut flags, "is_pic", "true")?;
    }
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
        eq_helpers: HashMap::new(),
        pending_eq: Vec::new(),
        thunks: HashMap::new(),
        pending_thunks: Vec::new(),
        fn_objects: HashMap::new(),
        workers: HashMap::new(),
        pending_workers: Vec::new(),
    };

    for (name, params, rets) in RUNTIME {
        let mut sig = be.module.make_signature();
        sig.params.extend(params.iter().map(|t| AbiParam::new(*t)));
        sig.returns.extend(rets.iter().map(|t| AbiParam::new(*t)));
        let id = be.module.declare_function(name, Linkage::Import, &sig).map_err(|e| e.to_string())?;
        be.runtime.insert(name, id);
    }
    for (i, f) in program.funcs.iter().enumerate() {
        let sig = be.func_signature(program, f);
        let id = match &f.external {
            // C's symbol; i64 and f64 pass as C's int64_t and double in the platform's C convention.
            Some(x) => be
                .module
                .declare_function(&x.symbol, Linkage::Import, &sig)
                .map_err(|e| format!("extern `{}` is declared twice with different signatures: {e}", x.symbol))?,
            None => {
                // Two modules may each have a private `helper`, so the index makes the symbol unique.
                let name = format!("anv{i}_{}", symbol_part(&f.name));
                be.module.declare_function(&name, Linkage::Local, &sig).map_err(|e| e.to_string())?
            }
        };
        be.funcs.push(id);
    }

    let mut fbc = FunctionBuilderContext::new();
    for (i, f) in program.funcs.iter().enumerate().filter(|(_, f)| f.external.is_none()) {
        let mut ctx = Context::new();
        ctx.set_disasm(asm);
        ctx.func.signature = be.func_signature(program, f);
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
    while let Some((id, elem, ret)) = be.pending_workers.pop() {
        let mut ctx = Context::new();
        ctx.func.signature = be.worker_signature();
        ctx.func.name = UserFuncName::user(4, id.as_u32());
        {
            let b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
            Gen::new(&mut be, program, b, &[]).worker_function(&elem, &ret);
        }
        be.module.define_function(id, &mut ctx).map_err(|e| {
            format!("codegen of parallel_map for {} -> {} failed: {e:?}", show(program, &elem), show(program, &ret))
        })?;
    }
    while let Some((fid, id)) = be.pending_thunks.pop() {
        let mut ctx = Context::new();
        ctx.func.signature = be.value_signature(program, &program.funcs[fid]);
        ctx.func.name = UserFuncName::user(3, id.as_u32());
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            b.seal_block(entry);
            // The closure (a static object) is not needed; the arguments pass through, owned.
            let params = b.block_params(entry)[1..].to_vec();
            let f = be.module.declare_func_in_func(be.funcs[fid], b.func);
            let call = b.ins().call(f, &params);
            let results = b.inst_results(call).to_vec();
            b.ins().return_(&results);
            b.finalize(be.module.isa().frontend_config());
        }
        be.module
            .define_function(id, &mut ctx)
            .map_err(|e| format!("codegen of the value of `{}` failed: {e:?}", program.funcs[fid].name))?;
    }
    // Helpers can ask for more (an array of arrays), so generate until none is pending.
    while let Some((id, ty)) = be.pending_eq.pop() {
        let mut ctx = Context::new();
        ctx.func.signature = be.eq_signature();
        ctx.func.name = UserFuncName::user(2, id.as_u32());
        {
            let b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
            Gen::new(&mut be, program, b, &[]).eq_function(&ty);
        }
        be.module
            .define_function(id, &mut ctx)
            .map_err(|e| format!("codegen of == for {} failed: {e:?}", show(program, &ty)))?;
    }
    match entry {
        Entry::Main => {
            if let Some(main) = program.main {
                be.c_main(program, main, &mut fbc)?;
            }
        }
        Entry::Tests(tests) => be.c_test_main(tests, &mut fbc)?,
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
        Ty::Str | Ty::Bytes | Ty::Array(_) | Ty::Map(..) | Ty::Fn(_) => out.push(Scalar { ty: I, heap: true }),
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
        Ty::Param(_) | Ty::Error => unreachable!("{ty:?} reached the backend"),
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

/// A name as part of a symbol: letters, digits and `_` (literals are named `main/fn@3:17`).
fn symbol_part(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect()
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

    /// How `f` is called directly. A function literal takes its closure first and reads its
    /// captured values from it; the rest is its own parameters.
    fn func_signature(&self, program: &Program, f: &Func) -> Signature {
        match f.captures {
            Some(_) => self.value_signature(program, f),
            None => self.signature(program, f.params.iter().map(|p| &f.locals[*p].ty), &f.ret),
        }
    }

    /// How a function value is called: (the closure, then the arguments).
    fn value_signature(&self, program: &Program, f: &Func) -> Signature {
        let own = &f.params[f.captures.unwrap_or(0)..];
        let mut sig = self.signature(program, own.iter().map(|p| &f.locals[*p].ty), &f.ret);
        sig.params.insert(0, AbiParam::new(I));
        sig
    }

    /// The value code of a declared function: a thunk, generated once all functions are.
    fn thunk(&mut self, program: &Program, fid: FuncId) -> ClFuncId {
        if let Some(id) = self.thunks.get(&fid) {
            return *id;
        }
        let sig = self.value_signature(program, &program.funcs[fid]);
        let name = format!("anv{fid}_{}_value", symbol_part(&program.funcs[fid].name));
        let id = self.module.declare_function(&name, Linkage::Local, &sig).expect("declare thunk");
        self.thunks.insert(fid, id);
        self.pending_thunks.push((fid, id));
        id
    }

    /// A static function object (refcount -1, never freed) with no captures, running `code`.
    fn fn_object(&mut self, code: ClFuncId) -> DataId {
        if let Some(id) = self.fn_objects.get(&code) {
            return *id;
        }
        let name = format!("anvil_fn_{}", self.fn_objects.len());
        let id = self.module.declare_data(&name, Linkage::Local, true, false).expect("declare function object");
        let mut bytes = Vec::new();
        bytes.extend((-1i64).to_le_bytes());
        bytes.extend(KIND_FN.to_le_bytes());
        bytes.extend([0u8; 16]);
        let mut desc = DataDescription::new();
        desc.set_align(8);
        desc.define(bytes.into_boxed_slice());
        let f = self.module.declare_func_in_data(code, &mut desc);
        desc.write_function_addr(FN_CODE as u32, f);
        self.module.define_data(id, &desc).expect("define function object");
        self.fn_objects.insert(code, id);
        id
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

    /// A parallel_map worker: (the function, its share of the input, the output, the share's first index).
    fn worker_signature(&self) -> Signature {
        let mut sig = self.module.make_signature();
        sig.params.extend([AbiParam::new(I); 4]);
        sig
    }

    fn eq_signature(&self) -> Signature {
        let mut sig = self.module.make_signature();
        sig.params.extend([AbiParam::new(I), AbiParam::new(I)]);
        sig.returns.push(AbiParam::new(types::I8));
        sig
    }

    /// Keys use the type itself, not its name: two modules may each declare a `Shape`.
    fn type_info(&mut self, program: &Program, elem: &Ty) -> DataId {
        self.layout_info(format!("{elem:?}"), layout(program, elem))
    }

    /// The element of a map: its key slot, then the value's.
    fn pair_info(&mut self, program: &Program, k: &Ty, v: &Ty) -> DataId {
        let mut lay = layout(program, k);
        lay.extend(layout(program, v));
        self.layout_info(format!("({k:?}, {v:?})"), lay)
    }

    /// Element layout for the runtime: slot size in bytes, then the byte offsets of heap pointers.
    fn layout_info(&mut self, key: String, lay: Vec<Scalar>) -> DataId {
        if let Some(id) = self.type_infos.get(&key) {
            return *id;
        }
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

    /// The C entry point of a test executable: runs the test named by its index in argv[1].
    fn c_test_main(&mut self, tests: &[(usize, FuncId)], fbc: &mut FunctionBuilderContext) -> Result<(), String> {
        let sig = self.module.make_signature();
        let id = self.module.declare_function(ENTRY, Linkage::Export, &sig).map_err(|e| e.to_string())?;
        let mut ctx = Context::new();
        ctx.func.signature = sig;
        ctx.func.name = UserFuncName::user(1, id.as_u32());
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, fbc);
            let entry = b.create_block();
            b.switch_to_block(entry);
            b.seal_block(entry);
            let index_of = self.module.declare_func_in_func(self.runtime["anvil_test_index"], b.func);
            let call = b.ins().call(index_of, &[]);
            let index = b.inst_results(call)[0];
            let done = b.create_block();
            for (i, fid) in tests {
                let run = b.create_block();
                let next = b.create_block();
                let hit = b.ins().icmp_imm_s(IntCC::Equal, index, *i as i64);
                b.ins().brif(hit, run, &[], next, &[]);
                b.switch_to_block(run);
                b.seal_block(run);
                let f = self.module.declare_func_in_func(self.funcs[*fid], b.func);
                b.ins().call(f, &[]);
                b.ins().jump(done, &[]);
                b.switch_to_block(next);
                b.seal_block(next);
            }
            let none = self.module.declare_func_in_func(self.runtime["anvil_no_test"], b.func);
            b.ins().call(none, &[index]);
            b.ins().jump(done, &[]);
            b.switch_to_block(done);
            b.seal_block(done);
            let exit = self.module.declare_func_in_func(self.runtime["anvil_exit"], b.func);
            let zero = b.ins().iconst(I, 0);
            b.ins().call(exit, &[zero]);
            b.ins().return_(&[]);
            b.finalize(self.module.isa().frontend_config());
        }
        self.module.define_function(id, &mut ctx).map_err(|e| format!("codegen of the test main failed: {e:?}"))
    }

    /// The program's entry: calls anv_main, validates the exit code, flushes and exits. The C
    /// runtime's `main` calls it (on a thread with a large stack outside Windows).
    fn c_main(&mut self, program: &Program, main: FuncId, fbc: &mut FunctionBuilderContext) -> Result<(), String> {
        let sig = self.module.make_signature();
        let id = self.module.declare_function(ENTRY, Linkage::Export, &sig).map_err(|e| e.to_string())?;
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
            b.ins().return_(&[]);
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
        ExprKind::Push { .. } | ExprKind::Pop { .. } | ExprKind::MapSet { .. } | ExprKind::MapRemove { .. } => true,
        ExprKind::Slice { base, from, to, .. } => mutates(base) || mutates(from) || mutates(to),
        ExprKind::MapLit { entries, .. } => entries.iter().any(|(k, v)| mutates(k) || mutates(v)),
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Str(_)
        | ExprKind::Local(_)
        | ExprKind::FnRef(_) => false,
        ExprKind::CallValue(callee, es) => mutates(callee) || any(es),
        ExprKind::Bits { l, r, .. } => mutates(l) || mutates(r),
        ExprKind::BitNot(x) => mutates(x),
        ExprKind::Closure { captures: es, .. } => any(es),
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
        let n = f.captures.unwrap_or(0);
        if f.captures.is_some() {
            // The closure is borrowed: each captured value is copied out with a reference of
            // its own, which the function releases at its end like any parameter's.
            let env = self.b.ins().iadd_imm_s(params[0], FN_ENV);
            let mut slot = 0;
            for p in &f.params[..n] {
                let lay = self.layouts[*p].clone();
                let base = self.b.ins().iadd_imm_s(env, slot * SLOT);
                let vals = self.load(base, &lay);
                self.retain(&lay, &vals);
                for (v, x) in self.vars[*p].clone().into_iter().zip(vals) {
                    self.b.def_var(v, x);
                }
                slot += lay.len() as i64;
            }
            i = 1;
        }
        for p in &f.params[n..] {
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
            // Coverage is measured in the interpreter.
            Stmt::Reached(_) => {}
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
            ExprKind::FnRef(fid) => {
                let code = self.be.thunk(self.program, *fid);
                let obj = self.be.fn_object(code);
                vec![self.data_ptr(obj)]
            }
            ExprKind::Closure { func, captures } if captures.is_empty() => {
                let obj = self.be.fn_object(self.be.funcs[*func]);
                vec![self.data_ptr(obj)]
            }
            ExprKind::Closure { func, captures } => {
                // The captured values move into the new object, after its header.
                let mut lay = Vec::new();
                let mut vals = Vec::new();
                for c in captures {
                    lay.extend(self.layout(&c.ty));
                    vals.extend(self.expr(c));
                }
                let key = format!("captures {:?}", captures.iter().map(|c| &c.ty).collect::<Vec<_>>());
                let info = self.be.layout_info(key, lay);
                let ti = self.data_ptr(info);
                let f = self.func_ref(*func);
                let code = self.b.ins().func_addr(I, f);
                let obj = self.call_rt("anvil_fn_new", &[ti, code]).unwrap();
                let env = self.b.ins().iadd_imm_s(obj, FN_ENV);
                self.store(env, &vals);
                vec![obj]
            }
            ExprKind::CallValue(callee, call_args) => {
                let (c, owned) = self.borrow(callee);
                let mut vals = vec![c[0]];
                for a in call_args {
                    vals.extend(self.expr(a));
                }
                let Ty::Fn(ft) = &callee.ty else { unreachable!() };
                let mut sig = self.be.signature(self.program, ft.params.iter(), &ft.ret);
                sig.params.insert(0, AbiParam::new(I));
                let sig = self.b.import_signature(sig);
                let code = self.b.ins().load(I, self.trusted, c[0], FN_CODE);
                let call = self.b.ins().call_indirect(sig, code, &vals);
                let results = self.b.inst_results(call).to_vec();
                if owned {
                    self.call_rt("anvil_release", &[c[0]]);
                }
                results
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
            ExprKind::Index { base, index, site } if base.ty == Ty::Bytes => {
                let (b, owned) = if mutates(index) { (self.expr(base), true) } else { self.borrow(base) };
                let i = self.expr(index)[0];
                let s = self.str_obj(site);
                let v = self.call_rt("anvil_bytes_at", &[b[0], i, s]).unwrap();
                if owned {
                    self.call_rt("anvil_release", &[b[0]]);
                }
                vec![v]
            }
            ExprKind::Bits { op, l, r, site } => {
                let a = self.expr(l)[0];
                let mut n = self.expr(r)[0];
                if matches!(op, BitOp::Shl | BitOp::Shr | BitOp::UShr) {
                    let s = self.str_obj(site);
                    n = self.call_rt("anvil_check_shift", &[n, s]).unwrap();
                }
                vec![match op {
                    BitOp::And => self.b.ins().band(a, n),
                    BitOp::Or => self.b.ins().bor(a, n),
                    BitOp::Xor => self.b.ins().bxor(a, n),
                    BitOp::Shl => self.b.ins().ishl(a, n),
                    BitOp::Shr => self.b.ins().sshr(a, n),
                    BitOp::UShr => self.b.ins().ushr(a, n),
                }]
            }
            ExprKind::BitNot(x) => {
                let v = self.expr(x)[0];
                vec![self.b.ins().bnot(v)]
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
            ExprKind::Slice { base, from, to, site } => {
                let (vals, owned) =
                    if mutates(from) || mutates(to) { (self.expr(base), true) } else { self.borrow(base) };
                let f = self.expr(from)[0];
                let t = self.expr(to)[0];
                let s = self.str_obj(site);
                let name = match base.ty {
                    Ty::Str => "anvil_str_slice",
                    Ty::Bytes => "anvil_bytes_slice",
                    _ => "anvil_arr_slice",
                };
                let out = self.call_rt(name, &[vals[0], f, t, s]).unwrap();
                if owned {
                    self.call_rt("anvil_release", &[vals[0]]);
                }
                vec![out]
            }
            ExprKind::MapLit { entries, duplicate } => {
                let Ty::Map(kt, vt) = &e.ty else { unreachable!() };
                let info = self.be.pair_info(self.program, kt, vt);
                let ti = self.data_ptr(info);
                let cap = self.b.ins().iconst(I, entries.len() as i64);
                let m = self.call_rt("anvil_arr_new", &[ti, cap]).unwrap();
                for (k, v) in entries {
                    let key = self.expr(k)[0];
                    let vals = self.expr(v);
                    let at = self.map_find(m, kt, key);
                    let found = self.b.ins().icmp_imm_s(IntCC::SignedGreaterThanOrEqual, at, 0);
                    self.panic_if(found, duplicate);
                    let pos = self.b.ins().bnot(at);
                    let slot = self.call_rt("anvil_arr_insert_slot", &[m, pos]).unwrap();
                    self.store(slot, &[key]);
                    let vslot = self.b.ins().iadd_imm_s(slot, SLOT);
                    self.store(vslot, &vals);
                }
                vec![m]
            }
            ExprKind::MapSet { local, path, key, value } => {
                let k = self.expr(key)[0];
                let vals = self.expr(value);
                let indices = self.indices(path);
                let loc = self.locate(*local, path, &indices);
                let m = self.unique_at(&loc);
                let at = self.map_find(m, &key.ty, k);
                let size = self.pair_size(&value.ty);
                let found = self.b.ins().icmp_imm_s(IntCC::SignedGreaterThanOrEqual, at, 0);
                let replace = self.b.create_block();
                let insert = self.b.create_block();
                let merge = self.b.create_block();
                self.b.ins().brif(found, replace, &[], insert, &[]);
                self.b.switch_to_block(replace);
                self.b.seal_block(replace);
                let addr = self.elem_at(m, at, size);
                let vaddr = self.b.ins().iadd_imm_s(addr, SLOT);
                let vlay = self.layout(&value.ty);
                let old = self.load(vaddr, &vlay);
                self.release(&vlay, &old);
                self.store(vaddr, &vals);
                // The map keeps its own key; the new one is dropped.
                let klay = self.layout(&key.ty);
                self.release(&klay, &[k]);
                self.b.ins().jump(merge, &[]);
                self.b.switch_to_block(insert);
                self.b.seal_block(insert);
                let pos = self.b.ins().bnot(at);
                let slot = self.call_rt("anvil_arr_insert_slot", &[m, pos]).unwrap();
                self.store(slot, &[k]);
                let vslot = self.b.ins().iadd_imm_s(slot, SLOT);
                self.store(vslot, &vals);
                self.b.ins().jump(merge, &[]);
                self.b.switch_to_block(merge);
                self.b.seal_block(merge);
                Vec::new()
            }
            ExprKind::MapRemove { local, path, key, missing } => {
                let k = self.expr(key)[0];
                let indices = self.indices(path);
                let loc = self.locate(*local, path, &indices);
                let (Loc::Vars(_, _, Ty::Map(_, vt)) | Loc::Mem(_, Ty::Map(_, vt))) = &loc else {
                    unreachable!("remove on a non-map place")
                };
                let vt = (**vt).clone();
                let m = self.unique_at(&loc);
                let at = self.map_find(m, &key.ty, k);
                let klay = self.layout(&key.ty);
                self.release(&klay, &[k]);
                let absent = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, at, 0);
                self.panic_if(absent, missing);
                let size = self.pair_size(&vt);
                let addr = self.elem_at(m, at, size);
                let mut lay = klay;
                lay.extend(self.layout(&vt));
                let old = self.load(addr, &lay);
                self.release(&lay, &old);
                self.call_rt("anvil_arr_remove_slot", &[m, at]);
                Vec::new()
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
            ExprKind::Cmp(op, l, r) if matches!(l.ty, Ty::Array(_) | Ty::Struct(_) | Ty::Enum(_) | Ty::Map(..)) => {
                let a = self.expr(l);
                let c = self.expr(r);
                let same = self.eq(&l.ty, &a, &c);
                let lay = self.layout(&l.ty);
                self.release(&lay, &a);
                self.release(&lay, &c);
                vec![if *op == CmpOp::Ne { self.b.ins().bxor_imm_u(same, 1) } else { same }]
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
                    Ty::Bytes => {
                        let same = self.call_rt("anvil_str_eq", &[a, c]).unwrap();
                        self.call_rt("anvil_release", &[a]);
                        self.call_rt("anvil_release", &[c]);
                        self.b.ins().icmp_imm_s(icc, same, 1)
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
            ExprKind::Builtin(bi, bargs) => self.builtin(bi, bargs, &e.ty),
        }
    }

    /// Pointer to a type info (or any data object).
    fn data_ptr(&mut self, data: DataId) -> Value {
        let gv = self.be.module.declare_data_in_func(data, self.b.func);
        self.b.ins().symbol_value(I, gv)
    }

    /// Bytes per element of a map whose values are `v`: the key slot, then the value's.
    fn pair_size(&self, v: &Ty) -> i64 {
        (1 + flat_len(self.program, v) as i64) * SLOT
    }

    /// Address of element `i` (in bounds) of an array or map with `size`-byte elements.
    fn elem_at(&mut self, arr: Value, i: Value, size: i64) -> Value {
        let data = self.b.ins().load(I, self.trusted, arr, ARR_DATA);
        let offset = self.b.ins().imul_imm_s(i, size);
        self.b.ins().iadd(data, offset)
    }

    /// Index of `key` in map `m`, or -(where it would go) - 1.
    fn map_find(&mut self, m: Value, kt: &Ty, key: Value) -> Value {
        let name = if *kt == Ty::Str { "anvil_map_find_str" } else { "anvil_map_find_i64" };
        self.call_rt(name, &[m, key]).unwrap()
    }

    /// `Some(some())` when `found`, otherwise `None`, as values of Option enum `eid`.
    fn option(&mut self, eid: EnumId, found: Value, some: impl FnOnce(&mut Self) -> Vec<Value>) -> Vec<Value> {
        self.either(eid, found, (OPTION_SOME, some), (OPTION_NONE, |_: &mut Self| Vec::new()))
    }

    /// Variant `yes` of enum `eid` when `cond` holds, else variant `no`; each closure gives its
    /// variant's fields, computed in its own branch.
    fn either(
        &mut self,
        eid: EnumId,
        cond: Value,
        (yes_variant, yes_fields): (usize, impl FnOnce(&mut Self) -> Vec<Value>),
        (no_variant, no_fields): (usize, impl FnOnce(&mut Self) -> Vec<Value>),
    ) -> Vec<Value> {
        let lay = self.layout(&Ty::Enum(eid));
        let yes = self.b.create_block();
        let no = self.b.create_block();
        let merge = self.b.create_block();
        let out: Vec<Value> = lay.iter().map(|s| self.b.append_block_param(merge, s.ty)).collect();
        self.b.ins().brif(cond, yes, &[], no, &[]);
        self.b.switch_to_block(yes);
        self.b.seal_block(yes);
        let vals = yes_fields(self);
        let v = self.enum_value(eid, yes_variant, &vals);
        self.b.ins().jump(merge, &args(&v));
        self.b.switch_to_block(no);
        self.b.seal_block(no);
        let vals = no_fields(self);
        let v = self.enum_value(eid, no_variant, &vals);
        self.b.ins().jump(merge, &args(&v));
        self.b.switch_to_block(merge);
        self.b.seal_block(merge);
        out
    }

    /// `a == b` for two values of `ty` (I8 1 or 0). Floats compare as IEEE values.
    fn eq(&mut self, ty: &Ty, a: &[Value], b: &[Value]) -> Value {
        match ty {
            Ty::I64 | Ty::Bool => self.b.ins().icmp(IntCC::Equal, a[0], b[0]),
            Ty::F64(_) => self.b.ins().fcmp(FloatCC::Equal, a[0], b[0]),
            Ty::Str | Ty::Bytes => {
                let r = self.call_rt("anvil_str_eq", &[a[0], b[0]]).unwrap();
                self.b.ins().icmp_imm_s(IntCC::NotEqual, r, 0)
            }
            Ty::Array(_) | Ty::Map(..) => {
                let f = self.eq_helper(ty);
                let call = self.b.ins().call(f, &[a[0], b[0]]);
                self.b.inst_results(call)[0]
            }
            Ty::Struct(sid) => {
                let parts: Vec<Ty> = self.program.structs[*sid].fields.iter().map(|(_, t)| t.clone()).collect();
                let mut acc = self.b.ins().iconst(types::I8, 1);
                self.eq_parts(&parts, a, b, 0, &mut acc);
                acc
            }
            Ty::Enum(eid) => {
                let parts: Vec<Ty> = self.program.enums[*eid]
                    .variants
                    .iter()
                    .flat_map(|v| v.fields.iter().map(|(_, t)| t.clone()))
                    .collect();
                let mut acc = self.b.ins().icmp(IntCC::Equal, a[0], b[0]);
                self.eq_parts(&parts, a, b, 1, &mut acc);
                acc
            }
            Ty::Void | Ty::Fn(_) | Ty::Param(_) | Ty::Error => unreachable!("== on {ty:?}"),
        }
    }

    /// ANDs into `acc` the equality of consecutive `parts` of two flattened values, from `start`.
    fn eq_parts(&mut self, parts: &[Ty], a: &[Value], b: &[Value], start: usize, acc: &mut Value) {
        let mut at = start;
        for t in parts {
            let n = flat_len(self.program, t);
            let same = self.eq(t, &a[at..at + n], &b[at..at + n]);
            *acc = self.b.ins().band(*acc, same);
            at += n;
        }
    }

    /// The generated `==` helper for an array or map type, declared on first use.
    fn eq_helper(&mut self, ty: &Ty) -> FuncRef {
        let key = format!("{ty:?}");
        let id = match self.be.eq_helpers.get(&key) {
            Some(id) => *id,
            None => {
                let sig = self.be.eq_signature();
                let name = format!("anvil_eq_{}", self.be.eq_helpers.len());
                let id = self.be.module.declare_function(&name, Linkage::Local, &sig).expect("declare == helper");
                self.be.eq_helpers.insert(key, id);
                self.be.pending_eq.push((id, ty.clone()));
                id
            }
        };
        self.be.module.declare_func_in_func(id, self.b.func)
    }

    /// The worker of parallel_map for `elem` -> `ret`, declared now and generated after the functions.
    fn worker(&mut self, elem: &Ty, ret: &Ty) -> FuncRef {
        let key = format!("{elem:?} -> {ret:?}");
        let id = match self.be.workers.get(&key) {
            Some(id) => *id,
            None => {
                let sig = self.be.worker_signature();
                let name = format!("anvil_worker_{}", self.be.workers.len());
                let id = self.be.module.declare_function(&name, Linkage::Local, &sig).expect("declare worker");
                self.be.workers.insert(key, id);
                self.be.pending_workers.push((id, elem.clone(), ret.clone()));
                id
            }
        };
        self.be.module.declare_func_in_func(id, self.b.func)
    }

    /// The body of a worker: the function on each element of its share, in order, each result
    /// stored at its index in the output; it stops when the runtime says an earlier element panicked.
    fn worker_function(mut self, elem: &Ty, ret: &Ty) {
        let entry = self.b.create_block();
        self.b.append_block_params_for_function_params(entry);
        self.b.switch_to_block(entry);
        self.b.seal_block(entry);
        let p = self.b.block_params(entry).to_vec();
        let (f, input, out, base) = (p[0], p[1], p[2], p[3]);
        let lay = self.layout(elem);
        let ret_size = flat_len(self.program, ret) as i64 * SLOT;
        let mut sig = self.be.signature(self.program, std::iter::once(elem), ret);
        sig.params.insert(0, AbiParam::new(I));
        let sig = self.b.import_signature(sig);
        let limit = self.b.declare_var(I);
        let n = self.b.ins().load(I, self.trusted, input, OBJ_LEN);
        self.b.def_var(limit, n);
        let zero = self.b.ins().iconst(I, 0);
        self.counted_loop(zero, limit, &[], |g, j| {
            let go = g.call_rt("anvil_parallel_at", &[j]).unwrap();
            let work = g.b.create_block();
            let stop = g.b.create_block();
            g.b.ins().brif(go, work, &[], stop, &[]);
            g.b.switch_to_block(stop);
            g.b.seal_block(stop);
            g.b.ins().return_(&[]);
            g.b.switch_to_block(work);
            g.b.seal_block(work);
            let addr = g.elem_at(input, j, lay.len() as i64 * SLOT);
            let vals = g.load(addr, &lay);
            g.retain(&lay, &vals);
            let code = g.b.ins().load(I, g.trusted, f, FN_CODE);
            let mut args = vec![f];
            args.extend(vals);
            let call = g.b.ins().call_indirect(sig, code, &args);
            let results = g.b.inst_results(call).to_vec();
            let i = g.b.ins().iadd(base, j);
            let dst = g.elem_at(out, i, ret_size);
            g.store(dst, &results);
        });
        self.b.ins().return_(&[]);
        self.b.seal_all_blocks();
        let config = self.be.module.isa().frontend_config();
        self.b.finalize(config);
    }

    /// The body of an `==` helper: same object, else same length and every element equal.
    fn eq_function(mut self, ty: &Ty) {
        let parts: Vec<Ty> = match ty {
            Ty::Array(elem) => vec![(**elem).clone()],
            Ty::Map(k, v) => vec![(**k).clone(), (**v).clone()],
            _ => unreachable!("== helper for {ty:?}"),
        };
        let size: i64 = parts.iter().map(|t| flat_len(self.program, t) as i64).sum::<i64>() * SLOT;
        let lay: Vec<Scalar> = parts.iter().flat_map(|t| self.layout(t)).collect();
        let entry = self.b.create_block();
        self.b.append_block_params_for_function_params(entry);
        self.b.switch_to_block(entry);
        self.b.seal_block(entry);
        let (a, c) = (self.b.block_params(entry)[0], self.b.block_params(entry)[1]);
        let yes = self.b.create_block();
        let no = self.b.create_block();
        let not_same = self.b.create_block();
        let both = self.b.create_block();
        let same = self.b.ins().icmp(IntCC::Equal, a, c);
        self.b.ins().brif(same, yes, &[], not_same, &[]);
        // Different objects: a null one (an unused enum field) equals nothing else.
        self.b.switch_to_block(not_same);
        self.b.seal_block(not_same);
        let an = self.b.ins().icmp_imm_s(IntCC::Equal, a, 0);
        let cn = self.b.ins().icmp_imm_s(IntCC::Equal, c, 0);
        let either = self.b.ins().bor(an, cn);
        self.b.ins().brif(either, no, &[], both, &[]);
        self.b.switch_to_block(both);
        self.b.seal_block(both);
        let la = self.b.ins().load(I, self.trusted, a, OBJ_LEN);
        let lc = self.b.ins().load(I, self.trusted, c, OBJ_LEN);
        let header = self.b.create_block();
        let body = self.b.create_block();
        let step = self.b.create_block();
        let counter = self.b.declare_var(I);
        let zero = self.b.ins().iconst(I, 0);
        self.b.def_var(counter, zero);
        let lens = self.b.ins().icmp(IntCC::NotEqual, la, lc);
        self.b.ins().brif(lens, no, &[], header, &[]);
        self.b.switch_to_block(header);
        let i = self.b.use_var(counter);
        let more = self.b.ins().icmp(IntCC::SignedLessThan, i, la);
        self.b.ins().brif(more, body, &[], yes, &[]);
        self.b.switch_to_block(body);
        self.b.seal_block(body);
        let i = self.b.use_var(counter);
        let pa = self.elem_at(a, i, size);
        let pc = self.elem_at(c, i, size);
        let va = self.load(pa, &lay);
        let vc = self.load(pc, &lay);
        let mut acc = self.b.ins().iconst(types::I8, 1);
        self.eq_parts(&parts, &va, &vc, 0, &mut acc);
        self.b.ins().brif(acc, step, &[], no, &[]);
        self.b.switch_to_block(step);
        self.b.seal_block(step);
        let i = self.b.use_var(counter);
        let next = self.b.ins().iadd_imm_s(i, 1);
        self.b.def_var(counter, next);
        self.b.ins().jump(header, &[]);
        self.b.seal_block(header);
        self.b.switch_to_block(yes);
        self.b.seal_block(yes);
        let t = self.b.ins().iconst(types::I8, 1);
        self.b.ins().return_(&[t]);
        self.b.switch_to_block(no);
        self.b.seal_block(no);
        let f = self.b.ins().iconst(types::I8, 0);
        self.b.ins().return_(&[f]);
        self.b.seal_all_blocks();
        let config = self.be.module.isa().frontend_config();
        self.b.finalize(config);
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

    fn builtin(&mut self, bi: &Builtin, bargs: &[Expr], ty: &Ty) -> Vec<Value> {
        match bi {
            Builtin::StrContains | Builtin::StrFind | Builtin::StrStartsWith | Builtin::StrEndsWith => {
                let s = self.expr(&bargs[0])[0];
                let t = self.expr(&bargs[1])[0];
                let name = match bi {
                    Builtin::StrStartsWith => "anvil_str_starts",
                    Builtin::StrEndsWith => "anvil_str_ends",
                    _ => "anvil_str_find",
                };
                let r = self.call_rt(name, &[s, t]).unwrap();
                self.call_rt("anvil_release", &[s]);
                self.call_rt("anvil_release", &[t]);
                return match bi {
                    Builtin::StrFind => {
                        let Ty::Enum(eid) = ty else { unreachable!() };
                        let found = self.b.ins().icmp_imm_s(IntCC::SignedGreaterThanOrEqual, r, 0);
                        self.option(*eid, found, |_| vec![r])
                    }
                    Builtin::StrContains => vec![self.b.ins().icmp_imm_s(IntCC::SignedGreaterThanOrEqual, r, 0)],
                    _ => vec![self.b.ins().icmp_imm_s(IntCC::NotEqual, r, 0)],
                };
            }
            Builtin::StrSplit { message } => {
                let s = self.expr(&bargs[0])[0];
                let sep = self.expr(&bargs[1])[0];
                let info = self.be.type_info(self.program, &Ty::Str);
                let ti = self.data_ptr(info);
                let m = self.str_obj(message);
                let out = self.call_rt("anvil_str_split", &[s, sep, ti, m]).unwrap();
                self.call_rt("anvil_release", &[s]);
                self.call_rt("anvil_release", &[sep]);
                return vec![out];
            }
            Builtin::MapHas | Builtin::MapGet => {
                let (vals, owned) =
                    if mutates(&bargs[1]) { (self.expr(&bargs[0]), true) } else { self.borrow(&bargs[0]) };
                let m = vals[0];
                let key = self.expr(&bargs[1])[0];
                let Ty::Map(kt, vt) = &bargs[0].ty else { unreachable!() };
                let (kt, vt) = ((**kt).clone(), (**vt).clone());
                let at = self.map_find(m, &kt, key);
                let klay = self.layout(&kt);
                self.release(&klay, &[key]);
                let found = self.b.ins().icmp_imm_s(IntCC::SignedGreaterThanOrEqual, at, 0);
                let out = if let Builtin::MapHas = bi {
                    vec![found]
                } else {
                    let Ty::Enum(eid) = ty else { unreachable!() };
                    let size = self.pair_size(&vt);
                    self.option(*eid, found, |g| {
                        let addr = g.elem_at(m, at, size);
                        let vaddr = g.b.ins().iadd_imm_s(addr, SLOT);
                        let vlay = g.layout(&vt);
                        let v = g.load(vaddr, &vlay);
                        g.retain(&vlay, &v);
                        v
                    })
                };
                if owned {
                    self.call_rt("anvil_release", &[m]);
                }
                return out;
            }
            Builtin::MapKeys | Builtin::MapValues => {
                let (vals, owned) = self.borrow(&bargs[0]);
                let m = vals[0];
                let Ty::Map(kt, vt) = &bargs[0].ty else { unreachable!() };
                let (part, offset) =
                    if let Builtin::MapKeys = bi { ((**kt).clone(), 0) } else { ((**vt).clone(), SLOT) };
                let size = self.pair_size(vt);
                let info = self.be.type_info(self.program, &part);
                let ti = self.data_ptr(info);
                let len = self.b.ins().load(I, self.trusted, m, OBJ_LEN);
                let out = self.call_rt("anvil_arr_new", &[ti, len]).unwrap();
                let limit = self.b.declare_var(I);
                self.b.def_var(limit, len);
                let zero = self.b.ins().iconst(I, 0);
                let lay = self.layout(&part);
                self.counted_loop(zero, limit, &[], |g, i| {
                    let addr = g.elem_at(m, i, size);
                    let addr = g.b.ins().iadd_imm_s(addr, offset);
                    let v = g.load(addr, &lay);
                    g.retain(&lay, &v);
                    let slot = g.call_rt("anvil_arr_push_slot", &[out]).unwrap();
                    g.store(slot, &v);
                });
                if owned {
                    self.call_rt("anvil_release", &[m]);
                }
                return vec![out];
            }
            Builtin::Print | Builtin::EPrint => {
                // Numbers and bools are written without building a str: print never allocates.
                let stream = self.b.ins().iconst(I, matches!(bi, Builtin::EPrint) as i64);
                match &bargs[0].ty {
                    Ty::I64 => {
                        let v = self.expr(&bargs[0]);
                        self.call_rt("anvil_print_i64", &[v[0], stream]);
                    }
                    Ty::Bool => {
                        let v = self.expr(&bargs[0]);
                        let wide = self.b.ins().uextend(I, v[0]);
                        self.call_rt("anvil_print_bool", &[wide, stream]);
                    }
                    Ty::F64(u) => {
                        let v = self.expr(&bargs[0]);
                        let unit = if u.is_dimensionless() { String::new() } else { format!(" {u}") };
                        let unit = self.str_obj(&unit);
                        self.call_rt("anvil_print_f64", &[v[0], unit, stream]);
                    }
                    _ => {
                        let (t, owned) = self.borrow(&bargs[0]);
                        self.call_rt("anvil_print_line", &[t[0], stream]);
                        if owned {
                            self.call_rt("anvil_release", &[t[0]]);
                        }
                    }
                }
                return Vec::new();
            }
            Builtin::Args => {
                let info = self.be.type_info(self.program, &Ty::Str);
                let ti = self.data_ptr(info);
                return vec![self.call_rt("anvil_args", &[ti]).unwrap()];
            }
            Builtin::ReadLine { message } => {
                let Ty::Enum(eid) = ty else { unreachable!() };
                let m = self.str_obj(message);
                let line = self.call_rt("anvil_read_line", &[m]).unwrap();
                let got = self.b.ins().icmp_imm_s(IntCC::NotEqual, line, 0);
                return self.option(*eid, got, |_| vec![line]);
            }
            Builtin::Exit { site } => {
                let code = self.expr(&bargs[0])[0];
                let s = self.str_obj(site);
                self.call_rt("anvil_exit_now", &[code, s]);
                return Vec::new();
            }
            Builtin::Env => {
                let Ty::Enum(eid) = ty else { unreachable!() };
                let name = self.expr(&bargs[0])[0];
                let v = self.call_rt("anvil_env", &[name]).unwrap();
                self.call_rt("anvil_release", &[name]);
                let got = self.b.ins().icmp_imm_s(IntCC::NotEqual, v, 0);
                return self.option(*eid, got, |_| vec![v]);
            }
            Builtin::ListDir => {
                let Ty::Enum(eid) = ty else { unreachable!() };
                let path = self.expr(&bargs[0])[0];
                let info = self.be.type_info(self.program, &Ty::Str);
                let ti = self.data_ptr(info);
                let r = self.call_rt("anvil_list_dir", &[path, ti]).unwrap();
                self.call_rt("anvil_release", &[path]);
                let ok = self.b.ins().icmp_imm_s(IntCC::NotEqual, r, 0);
                return self.either(
                    *eid,
                    ok,
                    (RESULT_OK, |_: &mut Self| vec![r]),
                    (RESULT_ERR, |g: &mut Self| vec![g.call_rt("anvil_file_error", &[]).unwrap()]),
                );
            }
            Builtin::ReadFile | Builtin::ReadBytes | Builtin::WriteFile | Builtin::WriteBytes => {
                // The runtime answers a str (null on failure) or the bytes written (-1); the
                // FileError variant of a failure is asked for after.
                let Ty::Enum(eid) = ty else { unreachable!() };
                let path = self.expr(&bargs[0])[0];
                let (r, ok) = if let Builtin::ReadFile | Builtin::ReadBytes = bi {
                    let name = if let Builtin::ReadFile = bi { "anvil_read_file" } else { "anvil_read_bytes" };
                    let r = self.call_rt(name, &[path]).unwrap();
                    (r, self.b.ins().icmp_imm_s(IntCC::NotEqual, r, 0))
                } else {
                    let text = self.expr(&bargs[1])[0];
                    let r = self.call_rt("anvil_write_file", &[path, text]).unwrap();
                    self.call_rt("anvil_release", &[text]);
                    (r, self.b.ins().icmp_imm_s(IntCC::SignedGreaterThanOrEqual, r, 0))
                };
                self.call_rt("anvil_release", &[path]);
                return self.either(
                    *eid,
                    ok,
                    (RESULT_OK, |_: &mut Self| vec![r]),
                    (RESULT_ERR, |g: &mut Self| vec![g.call_rt("anvil_file_error", &[]).unwrap()]),
                );
            }
            Builtin::StrTrim | Builtin::StrParseI64 | Builtin::StrParseF64 => {
                let s = self.expr(&bargs[0])[0];
                let out = match bi {
                    Builtin::StrTrim => vec![self.call_rt("anvil_str_trim", &[s]).unwrap()],
                    _ => {
                        let Ty::Enum(eid) = ty else { unreachable!() };
                        let name =
                            if let Builtin::StrParseI64 = bi { "anvil_str_parse_i64" } else { "anvil_str_parse_f64" };
                        let v = self.call_rt(name, &[s]).unwrap();
                        let ok = self.call_rt("anvil_parse_ok", &[]).unwrap();
                        let ok = self.b.ins().icmp_imm_s(IntCC::NotEqual, ok, 0);
                        self.option(*eid, ok, |_| vec![v])
                    }
                };
                self.call_rt("anvil_release", &[s]);
                return out;
            }
            Builtin::WrappingAdd | Builtin::WrappingSub | Builtin::WrappingMul => {
                let a = self.expr(&bargs[0])[0];
                let c = self.expr(&bargs[1])[0];
                return vec![match bi {
                    Builtin::WrappingAdd => self.b.ins().iadd(a, c),
                    Builtin::WrappingSub => self.b.ins().isub(a, c),
                    _ => self.b.ins().imul(a, c),
                }];
            }
            Builtin::BytesOf { site } => {
                let xs = self.expr(&bargs[0])[0];
                let s = self.str_obj(site);
                let out = self.call_rt("anvil_bytes_of", &[xs, s]).unwrap();
                self.call_rt("anvil_release", &[xs]);
                return vec![out];
            }
            // A str object is valid bytes as it is.
            Builtin::StrToBytes => return self.expr(&bargs[0]),
            Builtin::StrReplace { message } => {
                let s = self.expr(&bargs[0])[0];
                let old = self.expr(&bargs[1])[0];
                let new = self.expr(&bargs[2])[0];
                let m = self.str_obj(message);
                let out = self.call_rt("anvil_str_replace", &[s, old, new, m]).unwrap();
                for v in [s, old, new] {
                    self.call_rt("anvil_release", &[v]);
                }
                return vec![out];
            }
            Builtin::StrUpper | Builtin::StrLower | Builtin::StrChars | Builtin::ArrReversed => {
                let x = self.expr(&bargs[0])[0];
                let out = match bi {
                    Builtin::StrChars => {
                        let info = self.be.type_info(self.program, &Ty::Str);
                        let ti = self.data_ptr(info);
                        self.call_rt("anvil_str_chars", &[x, ti]).unwrap()
                    }
                    Builtin::ArrReversed => self.call_rt("anvil_arr_reversed", &[x]).unwrap(),
                    _ => {
                        let upper = self.b.ins().iconst(I, matches!(bi, Builtin::StrUpper) as i64);
                        self.call_rt("anvil_str_case", &[x, upper]).unwrap()
                    }
                };
                self.call_rt("anvil_release", &[x]);
                return vec![out];
            }
            // Each worker gets its own copies of the function and of its share of xs (see anvil_rt.c).
            Builtin::ParallelMap => {
                let xs = self.expr(&bargs[0])[0];
                let f = self.expr(&bargs[1])[0];
                let (Ty::Array(elem), Ty::Array(ret)) = (&bargs[0].ty, ty) else { unreachable!() };
                let worker = self.worker(elem, ret);
                let work = self.b.ins().func_addr(I, worker);
                let info = self.be.type_info(self.program, ret);
                let ti = self.data_ptr(info);
                let out = self.call_rt("anvil_parallel_map", &[xs, f, ti, work]).unwrap();
                self.call_rt("anvil_release", &[xs]);
                self.call_rt("anvil_release", &[f]);
                return vec![out];
            }
            Builtin::ArrJoin => {
                let xs = self.expr(&bargs[0])[0];
                let sep = self.expr(&bargs[1])[0];
                let out = self.call_rt("anvil_arr_join", &[xs, sep]).unwrap();
                self.call_rt("anvil_release", &[xs]);
                self.call_rt("anvil_release", &[sep]);
                return vec![out];
            }
            Builtin::FloatFixed { message } => {
                let v = self.expr(&bargs[0])[0];
                let n = self.expr(&bargs[1])[0];
                let m = self.str_obj(message);
                return vec![self.call_rt("anvil_f64_fixed", &[v, n, m]).unwrap()];
            }
            Builtin::ArrSorted { message } => {
                let xs = self.expr(&bargs[0])[0];
                let Ty::Array(elem) = &bargs[0].ty else { unreachable!() };
                let kind = match **elem {
                    Ty::I64 => 0,
                    Ty::F64(_) => 1,
                    _ => 2,
                };
                let kind = self.b.ins().iconst(I, kind);
                let m = self.str_obj(message);
                let out = self.call_rt("anvil_arr_sorted", &[xs, kind, m]).unwrap();
                self.call_rt("anvil_release", &[xs]);
                return vec![out];
            }
            Builtin::BytesToStr => {
                let Ty::Enum(eid) = ty else { unreachable!() };
                let bs = self.expr(&bargs[0])[0];
                let s = self.call_rt("anvil_bytes_to_str", &[bs]).unwrap();
                self.call_rt("anvil_release", &[bs]);
                let ok = self.b.ins().icmp_imm_s(IntCC::NotEqual, s, 0);
                return self.option(*eid, ok, |_| vec![s]);
            }
            Builtin::BytesHex => {
                let bs = self.expr(&bargs[0])[0];
                let s = self.call_rt("anvil_bytes_hex", &[bs]).unwrap();
                self.call_rt("anvil_release", &[bs]);
                return vec![s];
            }
            Builtin::BytesToArray => {
                let bs = self.expr(&bargs[0])[0];
                let info = self.be.type_info(self.program, &Ty::I64);
                let ti = self.data_ptr(info);
                let out = self.call_rt("anvil_bytes_to_array", &[bs, ti]).unwrap();
                self.call_rt("anvil_release", &[bs]);
                return vec![out];
            }
            Builtin::ClockNs => return vec![self.call_rt("anvil_clock_ns", &[]).unwrap()],
            Builtin::RandomF64 => return vec![self.call_rt("anvil_random_f64", &[]).unwrap()],
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
            Builtin::Print
            | Builtin::Len
            | Builtin::StrContains
            | Builtin::StrFind
            | Builtin::StrStartsWith
            | Builtin::StrEndsWith
            | Builtin::StrSplit { .. }
            | Builtin::MapHas
            | Builtin::MapGet
            | Builtin::MapKeys
            | Builtin::MapValues
            | Builtin::ClockNs
            | Builtin::RandomF64
            | Builtin::Args
            | Builtin::ReadLine { .. }
            | Builtin::ReadFile
            | Builtin::WriteFile
            | Builtin::StrTrim
            | Builtin::StrParseI64
            | Builtin::StrParseF64
            | Builtin::WrappingAdd
            | Builtin::WrappingSub
            | Builtin::WrappingMul
            | Builtin::BytesOf { .. }
            | Builtin::ReadBytes
            | Builtin::WriteBytes
            | Builtin::StrToBytes
            | Builtin::BytesToStr
            | Builtin::BytesHex
            | Builtin::BytesToArray
            | Builtin::StrReplace { .. }
            | Builtin::StrUpper
            | Builtin::StrLower
            | Builtin::StrChars
            | Builtin::ArrJoin
            | Builtin::FloatFixed { .. }
            | Builtin::ArrSorted { .. }
            | Builtin::ArrReversed
            | Builtin::EPrint
            | Builtin::Exit { .. }
            | Builtin::Env
            | Builtin::ListDir
            | Builtin::ParallelMap => unreachable!(),
            Builtin::RandomBelow { site } => {
                let site = self.str_obj(site);
                vec![self.call_rt("anvil_random_below", &[x, site]).unwrap()]
            }
            Builtin::RandomSeed => {
                self.call_rt("anvil_random_seed", &[x]);
                Vec::new()
            }
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
