//! Name resolution and type checking: AST in, typed IR out. Collects every error it can find
//! rather than stopping at the first, and never lets one error cause another (Ty::Error).

use crate::ast::{self, BinOp, Else, ExprKind as AK, InterpPart, StmtKind, TypeKind, UnOp};
use crate::diag::{self, Diagnostic, Edit, FixKind};
use crate::ir::*;
use crate::span::{SourceFile, Span};
use crate::units::Unit;
#[path = "check_generic.rs"]
mod generic;
#[path = "check_methods.rs"]
mod methods;

use generic::{Constraint, Generic, GenericKind, Pending, Scope};
use methods::{InterfaceDef, Method, MethodFn};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

pub const BUILTINS: &[&str] = &[
    "print",
    "print_raw",
    "assert",
    "panic",
    "sqrt",
    "abs",
    "to_f64",
    "to_i64",
    "clock_ns",
    "random_below",
    "random_f64",
    "random_seed",
    "args",
    "read_line",
    "read_stdin",
    "read_file",
    "write_file",
    "bytes_of",
    "read_bytes",
    "write_bytes",
    "eprint",
    "exit",
    "env",
    "list_dir",
];
/// The effects `uses` can list, in the order it lists them.
pub const EFFECTS: &[&str] = &["io", "fs", "clock", "random"];
/// Constructors of the built-in Option and Result enums, usable without a type prefix.
pub const CONSTRUCTORS: &[&str] = &["Some", "None", "Ok", "Err"];
pub const TYPE_NAMES: &[&str] = &["i64", "f64", "bool", "str", "bytes", "Option", "Result", "Map", "FileError", "Self"];

/// How completion shows each built-in function and constructor (same order as BUILTINS, then
/// CONSTRUCTORS; a test keeps them in step).
pub const BUILTIN_SIGNATURES: &[(&str, &str)] = &[
    ("print", "print(x) uses io: writes an i64, f64, bool or str and a newline"),
    ("print_raw", "print_raw(text: str) uses io: writes the text with no newline"),
    ("assert", "assert(cond: bool)"),
    ("panic", "panic(message: str): stops the program with the message; a branch that ends in it needs no test"),
    ("sqrt", "sqrt(x: f64) -> f64, halving the unit"),
    ("abs", "abs(x) for i64 or f64"),
    ("to_f64", "to_f64(i: i64) -> f64"),
    ("to_i64", "to_i64(x: f64) -> i64, truncating"),
    ("clock_ns", "clock_ns() -> i64 uses clock: nanoseconds since the program started"),
    ("random_below", "random_below(n: i64) -> i64 uses random: uniform in 0..n"),
    ("random_f64", "random_f64() -> f64 uses random: uniform in [0, 1)"),
    ("random_seed", "random_seed(seed: i64) uses random: restarts the sequence"),
    ("args", "args() -> [str] uses io: the command-line arguments"),
    ("read_line", "read_line() -> Option[str] uses io: the next line of stdin, None at the end"),
    ("read_stdin", "read_stdin() -> bytes uses io: all that is left of stdin"),
    ("read_file", "read_file(path: str) -> Result[str, FileError] uses fs: the whole file"),
    ("write_file", "write_file(path: str, text: str) -> Result[i64, FileError] uses fs: replaces the file"),
    ("bytes_of", "bytes_of(xs: [i64]) -> bytes: each value 0..=255 as a byte"),
    ("read_bytes", "read_bytes(path: str) -> Result[bytes, FileError] uses fs: the whole file"),
    ("write_bytes", "write_bytes(path: str, data: bytes) -> Result[i64, FileError] uses fs: replaces the file"),
    ("eprint", "eprint(x) uses io: like print, to stderr"),
    ("exit", "exit(code: i64) uses io: ends the program with the code"),
    ("env", "env(name: str) -> Option[str] uses io: an environment variable"),
    ("list_dir", "list_dir(path: str) -> Result[[str], FileError] uses fs: the names in a directory, sorted"),
    ("Some", "Some(value) -> Option[T]"),
    ("None", "None: Option[T]"),
    ("Ok", "Ok(value) -> Result[T, E]"),
    ("Err", "Err(error) -> Result[T, E]"),
];

/// The methods of the built-in types: (receiver kind, name, signature). `T` is an array's
/// element type, `K` and `V` a map's key and value types. A test calls every one.
pub const METHODS: &[(&str, &str, &str)] = &[
    ("str", "len", "len() -> i64"),
    ("str", "contains", "contains(t: str) -> bool"),
    ("str", "starts_with", "starts_with(t: str) -> bool"),
    ("str", "ends_with", "ends_with(t: str) -> bool"),
    ("str", "find", "find(t: str) -> Option[i64]"),
    ("str", "find_from", "find_from(t: str, at: i64) -> Option[i64]"),
    ("bytes", "word_at", "word_at(at: i64) -> i64"),
    ("str", "split", "split(sep: str) -> [str]"),
    ("str", "trim", "trim() -> str"),
    ("str", "parse_i64", "parse_i64() -> Option[i64]"),
    ("str", "parse_f64", "parse_f64() -> Option[f64]"),
    ("str", "to_bytes", "to_bytes() -> bytes"),
    ("str", "replace", "replace(old: str, new: str) -> str"),
    ("str", "to_upper", "to_upper() -> str"),
    ("str", "to_lower", "to_lower() -> str"),
    ("str", "chars", "chars() -> [str]"),
    ("array", "reversed", "reversed() -> [T]"),
    ("array", "sorted", "sorted() -> [T]"),
    ("array", "parallel_map", "parallel_map(f: fn(T) -> U) -> [U]"),
    ("strings", "join", "join(sep: str) -> str"),
    ("f64", "fixed", "fixed(digits: i64) -> str"),
    ("i64", "xor", "xor(other: i64) -> i64"),
    ("i64", "unsigned_shr", "unsigned_shr(n: i64) -> i64"),
    ("i64", "wrapping_add", "wrapping_add(other: i64) -> i64"),
    ("i64", "wrapping_sub", "wrapping_sub(other: i64) -> i64"),
    ("i64", "wrapping_mul", "wrapping_mul(other: i64) -> i64"),
    ("i64", "bit_len", "bit_len() -> i64"),
    ("i64", "leading_zeros", "leading_zeros() -> i64"),
    ("i64", "trailing_zeros", "trailing_zeros() -> i64"),
    ("bytes", "len", "len() -> i64"),
    ("bytes", "to_str", "to_str() -> Option[str]"),
    ("bytes", "hex", "hex() -> str"),
    ("bytes", "to_array", "to_array() -> [i64]"),
    ("array", "len", "len() -> i64"),
    ("array", "push", "push(x: T)"),
    ("array", "pop", "pop() -> T"),
    ("map", "len", "len() -> i64"),
    ("map", "get", "get(key: K) -> Option[V]"),
    ("map", "has", "has(key: K) -> bool"),
    ("map", "set", "set(key: K, value: V)"),
    ("map", "remove", "remove(key: K)"),
    ("map", "keys", "keys() -> [K]"),
    ("map", "values", "values() -> [V]"),
];
const INDENT: &str = "    ";

fn printable(ty: &Ty) -> bool {
    matches!(ty, Ty::I64 | Ty::F64(_) | Ty::Bool | Ty::Str)
}

/// The types `==` compares part by part; scalars and str have their own comparisons.
fn compound(ty: &Ty) -> bool {
    matches!(ty, Ty::Array(_) | Ty::Struct(_) | Ty::Enum(_) | Ty::Map(..) | Ty::Param(_))
}

struct FuncSig {
    params: Vec<(String, Ty)>,
    ret: Ty,
    effects: Effects,
    noalloc: bool,
    /// Where ` noalloc` would go in its signature (fixes that promise it).
    noalloc_insert: Span,
    span: Span,
    /// The module that declares it; `span` is in that module's file.
    module: usize,
}

/// One file of a program: its source, its syntax tree and, for each of its imports in order,
/// the index of the module it names. Module 0 is the program's root file.
pub struct Module<'a> {
    pub src: &'a SourceFile,
    pub file: &'a ast::File,
    /// `geometry` or `shapes.circle`; empty for the root.
    pub name: String,
    pub imports: Vec<usize>,
    /// The C files its extern functions name, by the text in quotes: the file's path and text,
    /// or why it could not be read.
    pub c_files: HashMap<String, Result<(String, String), String>>,
    /// The tests its doc comments' examples make (see `doc`).
    pub examples: &'a [ast::TestDecl],
}

/// What a checked module declares itself (never what it imports), and which of it is pub.
#[derive(Clone, Default)]
struct Exports {
    consts: HashMap<String, Const>,
    funcs: HashMap<String, FuncId>,
    structs: HashMap<String, StructId>,
    enums: HashMap<String, EnumId>,
    units: HashMap<String, Unit>,
    generic_fns: HashMap<String, usize>,
    generic_types: HashMap<String, usize>,
    interfaces: HashMap<String, usize>,
    public: HashSet<String>,
}

/// A constant: its value, which each use of it is, and where it is declared.
#[derive(Clone)]
pub(crate) struct Const {
    pub value: Expr,
    pub loc: Loc,
}

/// A place in one of the program's files: `file` indexes the files `analyze_with` returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Loc {
    pub file: usize,
    pub span: Span,
}

/// A declared name: functions, types, fields, variants, units, parameters and locals.
#[derive(Clone, Debug)]
pub struct Def {
    /// The file it is declared in: the root (0), or a module the program imports.
    pub file: usize,
    /// The span of the name where it is declared; uses point here.
    pub span: Span,
    /// `area`, `Shape`, `Shape.Circle`, `Body.mass`, `Shape.Circle.radius`, or a local's name.
    pub name: String,
    /// fn, type (a generic struct or enum), struct, enum, field, variant, unit, param or local.
    pub kind: &'static str,
    /// The declaration's source text for items; the type for fields, parameters and locals.
    pub detail: String,
    /// For parameters and locals: where the name can be used, from its declaration to the end
    /// of its block.
    pub scope: Option<Span>,
    /// For items, fields and variants: the doc comment above the declaration (see `doc`).
    pub doc: Option<String>,
}

impl Def {
    pub fn loc(&self) -> Loc {
        Loc { file: self.file, span: self.span }
    }
}

/// What the checker learned about a file, kept even when it has errors: every declaration in
/// it and every item declared in the modules it imports, every use of a declared name in it,
/// and the type of every expression in it. `anvil type-at`, `refs` and `complete` answer from
/// these.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub defs: Vec<Def>,
    /// (span of the use, the declaration it names)
    pub uses: Vec<(Span, Loc)>,
    /// (span of the expression, its type as shown in errors)
    pub exprs: Vec<(Span, String)>,
}

/// Everything `check` finds; `program` is None when there are errors.
pub struct Analysis {
    pub program: Option<Program>,
    pub diags: Vec<Diagnostic>,
    pub facts: Facts,
}

pub struct Checker<'a> {
    src: &'a SourceFile,
    /// The C files of the program, and the index of each by path.
    c_files: Vec<CFile>,
    c_index: HashMap<String, usize>,
    /// The current module's C files by the text in quotes: an index into c_files, or why not.
    module_c: HashMap<String, Result<usize, String>>,
    /// The module being checked; only the root's (0) facts are kept.
    module: usize,
    /// Filled in as each module is checked, dependencies first.
    exports: Vec<Option<Exports>>,
    /// Names the current module imports: (where the import names it, which import item).
    imported: HashMap<String, (Span, usize)>,
    /// Imported names the current module has used.
    touched: RefCell<HashSet<String>>,
    /// Names whose import failed; their uses are not reported again.
    failed_imports: HashSet<String>,
    /// (module, pub) of each struct; of each enum, None for Option/Result instantiations.
    struct_owner: Vec<(usize, bool)>,
    enum_owner: Vec<Option<(usize, bool)>>,
    /// Generic declarations, the names the current module sees them by, and their instances
    /// ((generic, type arguments) to the FuncId, StructId or EnumId made for them).
    generics: Vec<Generic<'a>>,
    generic_fns: HashMap<String, usize>,
    generic_types: HashMap<String, usize>,
    instances: HashMap<(usize, String), usize>,
    struct_inst: HashMap<StructId, (usize, Vec<Ty>)>,
    enum_inst: HashMap<EnumId, (usize, Vec<Ty>)>,
    /// The type arguments of each Option/Result instantiation.
    builtin_args: HashMap<EnumId, Vec<Ty>>,
    /// What each type parameter name means right now (a concrete type in an instance,
    /// `Ty::Param` in a generic body), and the constraints of those being checked abstractly.
    type_params: HashMap<String, Ty>,
    param_constraints: HashMap<String, generic::TypeParam>,
    /// True while a generic body is checked against its constraints (its IR is dropped).
    abstract_body: bool,
    pending: Vec<Pending>,
    /// While above 0, errors go to `quiet_errors` and no facts are kept.
    quiet: usize,
    quiet_errors: Vec<Diagnostic>,
    depth: usize,
    depth_reported: bool,
    /// The level of the instance whose body is being checked (0 outside instances).
    inst_level: usize,
    /// Each module's name tables once it is checked, and its name.
    scopes: Vec<Option<Scope>>,
    module_names: Vec<String>,
    diags: Vec<Diagnostic>,
    facts: Facts,
    /// Declaration spans of types, struct fields, enum variants and their fields, and units.
    type_spans: HashMap<String, Loc>,
    field_spans: HashMap<(StructId, usize), Loc>,
    variant_spans: HashMap<(EnumId, usize), Loc>,
    variant_field_spans: HashMap<(EnumId, usize, usize), Loc>,
    unit_spans: HashMap<String, Loc>,
    units: HashMap<String, Unit>,
    consts: HashMap<String, Const>,
    /// The constants this module's code reads.
    used_consts: HashSet<String>,
    /// Keep the line of each statement (`Stmt::Line`), for `profile`: the lines, and their indices.
    keep_lines: bool,
    lines: Vec<String>,
    line_ids: HashMap<(usize, usize), usize>,
    structs: Vec<StructDef>,
    struct_index: HashMap<String, StructId>,
    enums: Vec<EnumDef>,
    enum_index: HashMap<String, EnumId>,
    /// Instantiations of Option/Result, by their type arguments (names alone could clash
    /// between modules).
    builtin_enums: HashMap<String, EnumId>,
    /// The built-in `FileError`, which every module sees.
    file_error: EnumId,
    /// Function literals' bodies, lifted into functions of their own, by FuncId.
    lifted: Vec<(FuncId, Func)>,
    /// Every branch so far, and each one's id by (module, start of its block): an instance's
    /// body reuses its generic's.
    branches: Vec<Branch>,
    branch_ids: HashMap<(usize, usize), BranchId>,
    /// Every interface, and the names the current module sees them by.
    interfaces: Vec<InterfaceDef>,
    interface_index: HashMap<String, usize>,
    /// Methods by the type they are declared on (see `method_owner`), then by name.
    methods: HashMap<String, HashMap<String, Method>>,
    sigs: Vec<FuncSig>,
    func_index: HashMap<String, FuncId>,
}

struct LocalMeta {
    span: Span,
    kw_span: Option<Span>,
    mutable: bool,
    is_param: bool,
    is_loop_var: bool,
    used: bool,
    assigned: bool,
    /// Replaces the default "rename to _x" fix for an unused binding (match patterns).
    rename_fix: Option<(Span, String)>,
}

/// Per-function (or per-test) state while checking a body.
struct Body {
    locals: Vec<Local>,
    meta: Vec<LocalMeta>,
    scopes: Vec<HashMap<String, LocalId>>,
    declared: HashMap<String, Span>,
    /// The fact recorded for each local, so its scope can be filled in when its block ends.
    facts: HashMap<LocalId, usize>,
    ret: Ty,
    uses: Effects,
    used: Effects,
    /// The effect names as written, and the whole clause, for fixes.
    uses_list: Vec<ast::Ident>,
    uses_span: Option<Span>,
    uses_insert: Span,
    /// Some(the `noalloc` keyword) when the body must not allocate.
    noalloc: Option<Span>,
    loop_depth: usize,
    /// Some(local) while checking ensures clauses of a function with a return value.
    result_local: Option<LocalId>,
    in_ensures: bool,
    in_contract: bool,
    /// What lambdas are named after: the function or test this body belongs to.
    name: String,
    /// In a function literal: the locals of the functions around it, which its body may read
    /// (their type and where they are declared). Reading one captures it.
    enclosing: Option<HashMap<String, (Ty, Span)>>,
    /// The locals made for captured values, in the order they were first read.
    captures: Vec<LocalId>,
    /// The name of the declared function whose branches these are; None in a test.
    owner: Option<Span>,
}

impl Body {
    /// Whether `name` is a local here, or one this function literal could capture.
    fn sees(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains_key(name))
            || self.enclosing.as_ref().is_some_and(|e| e.contains_key(name))
    }
}

pub fn check(src: &SourceFile, file: &ast::File) -> Result<Program, Vec<Diagnostic>> {
    let a = analyze(src, file);
    match a.program {
        Some(p) => Ok(p),
        None => Err(a.diags),
    }
}

/// A program of one file. Its imports find no module (E0601); `analyze_modules` is the way to
/// check a file with imports.
pub fn analyze(src: &SourceFile, file: &ast::File) -> Analysis {
    let m = Module { src, file, name: String::new(), imports: Vec::new(), c_files: HashMap::new(), examples: &[] };
    analyze_modules(&[m], &[0], false)
}

/// Checks the modules of a program in `order` (each after the modules it imports, the root
/// last) and joins them into one program; diagnostics carry the index of their module's file.
pub fn analyze_modules<'a>(modules: &[Module<'a>], order: &[usize], keep_lines: bool) -> Analysis {
    let srcs: Vec<&'a SourceFile> = modules.iter().map(|m| m.src).collect();
    let mut c = Checker {
        src: modules[0].src,
        c_files: Vec::new(),
        c_index: HashMap::new(),
        module_c: HashMap::new(),
        module: 0,
        exports: vec![None; modules.len()],
        imported: HashMap::new(),
        touched: RefCell::new(HashSet::new()),
        failed_imports: HashSet::new(),
        struct_owner: Vec::new(),
        enum_owner: Vec::new(),
        generics: Vec::new(),
        generic_fns: HashMap::new(),
        generic_types: HashMap::new(),
        instances: HashMap::new(),
        struct_inst: HashMap::new(),
        enum_inst: HashMap::new(),
        builtin_args: HashMap::new(),
        type_params: HashMap::new(),
        param_constraints: HashMap::new(),
        abstract_body: false,
        pending: Vec::new(),
        quiet: 0,
        quiet_errors: Vec::new(),
        depth: 0,
        depth_reported: false,
        inst_level: 0,
        scopes: vec![None; modules.len()],
        module_names: modules.iter().map(|m| m.name.clone()).collect(),
        diags: Vec::new(),
        facts: Facts::default(),
        type_spans: HashMap::new(),
        field_spans: HashMap::new(),
        variant_spans: HashMap::new(),
        variant_field_spans: HashMap::new(),
        unit_spans: HashMap::new(),
        units: HashMap::new(),
        consts: HashMap::new(),
        used_consts: HashSet::new(),
        keep_lines,
        lines: Vec::new(),
        line_ids: HashMap::new(),
        structs: Vec::new(),
        struct_index: HashMap::new(),
        enums: Vec::new(),
        enum_index: HashMap::new(),
        builtin_enums: HashMap::new(),
        file_error: 0,
        lifted: Vec::new(),
        branches: Vec::new(),
        branch_ids: HashMap::new(),
        interfaces: Vec::new(),
        interface_index: HashMap::new(),
        methods: HashMap::new(),
        sigs: Vec::new(),
        func_index: HashMap::new(),
    };
    let variants = FILE_ERRORS.iter().map(|v| VariantDef { name: v.to_string(), fields: Vec::new() }).collect();
    c.file_error = match c.builtin_enum("FileError".into(), "FileError".into(), variants, Vec::new()) {
        Ty::Enum(id) => id,
        _ => unreachable!(),
    };
    let mut funcs = Vec::new();
    let mut tests = Vec::new();
    let mut main = None;
    for &m in order {
        c.begin_module(m, modules[m].src);
        let (fs, ts, mn) = c.module(&modules[m], &srcs);
        funcs.extend(fs);
        if m == 0 {
            tests = ts;
            main = mn;
        }
    }
    // Function literals were given FuncIds as they were met, between the declared functions
    // and the instances: put every function in FuncId order.
    funcs.append(&mut c.lifted);
    funcs.sort_by_key(|(id, _)| *id);
    if c.diags.is_empty() {
        assert!(funcs.iter().enumerate().all(|(i, (id, _))| i == *id), "FuncIds have a gap");
    }
    let funcs: Vec<Func> = funcs.into_iter().map(|(_, f)| f).collect();
    // Layout last, and only for an otherwise clean program: a broken file is fixed first.
    if c.diags.is_empty() {
        for (m, module) in modules.iter().enumerate() {
            if let Some(mut d) = crate::fmt::check_layout(module.src, module.file) {
                d.file = m;
                c.diags.push(d);
            }
        }
    }
    c.diags.sort_by_key(|d| (d.file, d.span.start, d.code));
    let generic_fns = c
        .generics
        .iter()
        .filter_map(|g| match g.kind {
            GenericKind::Fn(d) => Some(GenericFn {
                name: g.name.clone(),
                module: g.module,
                span: d.name.span,
                allows: d.allows.iter().map(|a| (a.lint.name.clone(), a.reason.clone())).collect(),
            }),
            _ => None,
        })
        .collect();
    let mut program = Program {
        structs: c.structs.clone(),
        enums: c.enums.clone(),
        funcs,
        tests,
        main,
        modules: modules.iter().map(|m| m.src.name.clone()).collect(),
        generic_fns,
        c_files: c.c_files.clone(),
        branches: c.branches.clone(),
        handed: Default::default(),
        lines: c.lines.clone(),
    };
    if c.diags.is_empty() {
        crate::moves::mark(&mut program);
    }
    let program = c.diags.is_empty().then_some(program);
    Analysis { program, diags: c.diags, facts: c.facts }
}

/// Edit distance counting a swap of adjacent letters as one edit (optimal string alignment),
/// so typos like `cuont` for `count` get suggestions.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = (a[i - 1] != b[j - 1]) as usize;
            let mut v = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = v;
        }
    }
    d[a.len()][b.len()]
}

/// The candidate within a small edit distance of `name`, for "did you mean" suggestions.
/// Where ` noalloc` goes in the signature of `d`: after its uses clause, or where one would go.
fn noalloc_insert(d: &ast::FnDecl) -> Span {
    match d.uses_span {
        Some(s) => s.at_end(),
        None => d.uses_insert,
    }
}

pub fn closest<'b>(name: &str, candidates: impl Iterator<Item = &'b String>) -> Option<String> {
    let limit = (name.len() / 3).max(1);
    candidates
        .filter(|c| c.as_str() != name)
        .map(|c| (distance(name, c), c))
        .filter(|(d, _)| *d <= limit)
        .min()
        .map(|(_, c)| c.clone())
}

impl<'a> Checker<'a> {
    fn err(&mut self, mut d: Diagnostic) {
        d.file = self.module;
        if self.quiet > 0 {
            self.quiet_errors.push(d);
        } else {
            self.diags.push(d);
        }
    }

    /// Records a declaration: every one in the root, and the items of imported modules. Returns
    /// its index in the facts.
    fn def(
        &mut self,
        span: Span,
        name: impl Into<String>,
        kind: &'static str,
        detail: impl Into<String>,
    ) -> Option<usize> {
        let item = !matches!(kind, "param" | "local");
        if self.quiet > 0 || (self.module != 0 && !item) {
            return None;
        }
        let doc = match kind {
            "param" | "local" => None,
            _ => crate::doc::doc_above(&self.src.text, span.start),
        };
        let def = Def { file: self.module, span, name: name.into(), kind, detail: detail.into(), scope: None, doc };
        self.facts.defs.push(def);
        Some(self.facts.defs.len() - 1)
    }

    /// A place in the module being checked.
    fn here(&self, span: Span) -> Loc {
        Loc { file: self.module, span }
    }

    /// Ends a scope of `b`: its locals can be used up to `end`.
    fn close_scope(&mut self, b: &mut Body, end: usize) {
        let scope = b.scopes.pop().expect("an open scope");
        for id in scope.values() {
            if let Some(&i) = b.facts.get(id) {
                let d = &mut self.facts.defs[i];
                d.scope = Some(Span::new(d.span.start, end));
            }
        }
    }

    fn note_use(&mut self, span: Span, def: Option<Loc>) {
        if let (Some(def), 0, 0) = (def, self.module, self.quiet) {
            self.facts.uses.push((span, def));
        }
    }

    /// Records that the current module used `name`, for the unused-import check.
    fn touch(&self, name: &str) {
        if self.imported.contains_key(name) {
            self.touched.borrow_mut().insert(name.to_string());
        }
    }

    fn show(&self, ty: &Ty) -> String {
        TyDisplay { ty, structs: &self.structs, enums: &self.enums }.to_string()
    }

    fn site(&self, span: Span) -> String {
        self.src.site(span)
    }

    fn text(&self, span: Span) -> String {
        self.src.slice(span).to_string()
    }

    // ---------------- declarations ----------------

    /// Starts checking module `m`: its own names, nothing imported yet.
    fn begin_module(&mut self, m: usize, src: &'a SourceFile) {
        self.module = m;
        self.src = src;
        self.func_index.clear();
        self.struct_index.clear();
        self.enum_index.clear();
        self.units.clear();
        self.type_spans.clear();
        self.unit_spans.clear();
        self.consts.clear();
        self.used_consts.clear();
        self.imported.clear();
        self.touched.borrow_mut().clear();
        self.failed_imports.clear();
        self.generic_fns.clear();
        self.generic_types.clear();
        self.interface_index.clear();
    }

    /// Checks one module: its imports, declarations and function bodies (and, for the root,
    /// its tests and main). Returns its functions in FuncId order.
    fn module(&mut self, me: &Module<'a>, srcs: &[&'a SourceFile]) -> (Vec<(FuncId, Func)>, Vec<Test>, Option<FuncId>) {
        let file: &'a ast::File = me.file;
        let mut import_decls = Vec::new();
        let mut unit_decls = Vec::new();
        let mut const_decls = Vec::new();
        let mut struct_decls = Vec::new();
        let mut enum_decls = Vec::new();
        let mut fn_decls = Vec::new();
        let mut test_decls = Vec::new();
        let mut generic_decls = Vec::new();
        let mut interface_decls = Vec::new();
        for item in &file.items {
            match item {
                ast::Item::Import(i) => import_decls.push(i),
                ast::Item::Interface(i) => interface_decls.push(i),
                ast::Item::Unit(u) => unit_decls.push(u),
                ast::Item::Const(c) => const_decls.push(c),
                ast::Item::Struct(s) if !s.type_params.is_empty() => generic_decls.push(GenericKind::Struct(s)),
                ast::Item::Enum(e) if !e.type_params.is_empty() => generic_decls.push(GenericKind::Enum(e)),
                ast::Item::Fn(f) if f.external.is_some() && !f.type_params.is_empty() => {
                    self.err(
                        Diagnostic::new("E0800", f.name.span, "an extern function cannot be generic")
                            .note("C has one copy of each function: declare one extern per type it takes", None),
                    );
                }
                ast::Item::Fn(f) if !f.type_params.is_empty() => generic_decls.push(GenericKind::Fn(f)),
                ast::Item::Struct(s) => struct_decls.push(s),
                ast::Item::Enum(e) => enum_decls.push(e),
                ast::Item::Fn(f) => fn_decls.push(&**f),
                ast::Item::Test(t) => test_decls.push(t),
            }
        }
        test_decls.extend(me.examples.iter());
        for (k, imp) in import_decls.iter().enumerate() {
            self.import(imp, k, me.imports.get(k).copied());
        }
        self.module_c.clear();
        for (written, file) in &me.c_files {
            let entry = match file {
                Ok((path, text)) => Ok(*self.c_index.entry(path.clone()).or_insert_with(|| {
                    self.c_files.push(CFile { name: path.clone(), text: text.clone() });
                    self.c_files.len() - 1
                })),
                Err(why) => Err(why.clone()),
            };
            self.module_c.insert(written.clone(), entry);
        }
        self.declare_units(&unit_decls);
        // Interface names before the type parameters that name them as constraints.
        self.declare_interfaces(&interface_decls);
        // Generic names first, so other declarations can use them; their shapes once all names exist.
        let first_generic = self.generics.len();
        let generic_methods = self.declare_generics(&generic_decls);
        self.declare_types(&struct_decls, &enum_decls);
        // Generic methods are recorded on their types once every type of the module is declared.
        for (d, g) in generic_methods {
            self.register_method(d, MethodFn::Generic(g));
        }
        let sig_base = self.sigs.len();
        self.declare_fns(&fn_decls);
        self.declare_consts(&const_decls);
        self.resolve_interfaces(&interface_decls);
        let own_generics: Vec<usize> = (first_generic..self.generics.len()).collect();
        for &g in &own_generics {
            self.resolve_shape(g);
        }
        for &g in &own_generics {
            if !matches!(self.generics[g].kind, GenericKind::Fn(_)) {
                self.check_generic_type(g);
            }
        }
        self.check_public(&struct_decls, &enum_decls, &fn_decls, sig_base, &own_generics);
        for &g in &own_generics {
            self.check_generic_body(g);
        }

        let mut funcs = Vec::new();
        for f in &fn_decls {
            let (id, mut func) = self.func(f);
            if let Some(r) = &f.receiver {
                func.name = format!("{}.{}", r.ty.name, func.name);
            }
            if !me.name.is_empty() {
                func.name = format!("{}.{}", me.name, func.name);
            }
            funcs.push((id, func));
        }
        let mut tests = Vec::new();
        let mut main = None;
        if self.module == 0 {
            let mut seen_tests: HashMap<String, Span> = HashMap::new();
            for t in &test_decls {
                if let Some(prev) = seen_tests.insert(t.name.clone(), t.name_span) {
                    self.err(
                        Diagnostic::new("E0201", t.name_span, format!("test \"{}\" is declared twice", t.name))
                            .note("first declared here", Some(prev)),
                    );
                }
                tests.push(self.test(t));
            }
            main = self.func_index.get("main").copied().filter(|id| self.sigs[*id].module == 0);
            if let Some(id) = main {
                let sig = &self.sigs[id];
                let ok = sig.params.is_empty() && matches!(sig.ret, Ty::Void | Ty::I64 | Ty::Error);
                if !ok {
                    let span = sig.span;
                    self.err(Diagnostic::new("E0350", span, "main takes no parameters and returns nothing or i64"));
                }
            }
        } else if let Some(f) = fn_decls.iter().find(|f| f.name.name == "main") {
            self.err(
                Diagnostic::new("E0606", f.name.span, format!("module `{}` defines main", me.name))
                    .note("only the program's root file defines main; this file is imported", None),
            );
        }
        self.unused_imports(&import_decls);
        // Instances asked for by this module's bodies, checked where their generics live.
        self.scopes[self.module] = Some(self.save_scope());
        funcs.extend(self.run_pending(srcs));
        if self.module == 0 {
            for d in &const_decls {
                let name = &d.name.name;
                if !d.public && !name.starts_with('_') && !self.used_consts.contains(name) && self.consts.contains_key(name) {
                    self.err(Diagnostic::new("E0340", d.name.span, format!("`{name}` is never used")));
                }
            }
        }

        let mut ex = Exports::default();
        let own = |name: &String| !self.imported.contains_key(name);
        ex.consts = self.consts.iter().filter(|(n, _)| own(n)).map(|(n, v)| (n.clone(), v.clone())).collect();
        ex.funcs = self.func_index.iter().filter(|(n, _)| own(n)).map(|(n, v)| (n.clone(), *v)).collect();
        ex.structs = self.struct_index.iter().filter(|(n, _)| own(n)).map(|(n, v)| (n.clone(), *v)).collect();
        ex.enums = self.enum_index.iter().filter(|(n, _)| own(n)).map(|(n, v)| (n.clone(), *v)).collect();
        ex.units = self.units.iter().filter(|(n, _)| own(n)).map(|(n, v)| (n.clone(), v.clone())).collect();
        ex.generic_fns = self.generic_fns.iter().filter(|(n, _)| own(n)).map(|(n, v)| (n.clone(), *v)).collect();
        ex.generic_types = self.generic_types.iter().filter(|(n, _)| own(n)).map(|(n, v)| (n.clone(), *v)).collect();
        ex.interfaces = self.interface_index.iter().filter(|(n, _)| own(n)).map(|(n, v)| (n.clone(), *v)).collect();
        for item in &file.items {
            let name = match item {
                ast::Item::Unit(d) if d.public => &d.name.name,
                ast::Item::Const(d) if d.public => &d.name.name,
                ast::Item::Struct(d) if d.public => &d.name.name,
                ast::Item::Enum(d) if d.public => &d.name.name,
                ast::Item::Fn(d) if d.public && d.receiver.is_none() => &d.name.name,
                ast::Item::Interface(d) if d.public => &d.name.name,
                _ => continue,
            };
            ex.public.insert(name.clone());
        }
        self.exports[self.module] = Some(ex);
        (funcs, tests, main)
    }

    /// Brings the names of one import into the current module.
    fn import(&mut self, imp: &ast::ImportDecl, k: usize, target: Option<usize>) {
        let module = imp.module();
        let Some(ex) = target.and_then(|t| self.exports[t].clone()) else {
            self.failed_imports.extend(imp.names.iter().map(|n| n.name.clone()));
            if target.is_none() {
                self.err(Diagnostic::new("E0601", imp.path_span(), format!("cannot find module `{module}`")));
            }
            return;
        };
        for n in &imp.names {
            let name = &n.name;
            if let Some((prev, _)) = self.imported.get(name) {
                let prev = *prev;
                self.err(
                    Diagnostic::new("E0201", n.span, format!("`{name}` is imported twice"))
                        .note("first imported here", Some(prev)),
                );
                continue;
            }
            let mut found = false;
            if let Some(c) = ex.consts.get(name) {
                self.consts.insert(name.clone(), c.clone());
                found = true;
            }
            if let Some(id) = ex.funcs.get(name) {
                self.func_index.insert(name.clone(), *id);
                found = true;
            }
            if let Some(id) = ex.structs.get(name) {
                self.struct_index.insert(name.clone(), *id);
                found = true;
            }
            if let Some(id) = ex.enums.get(name) {
                self.enum_index.insert(name.clone(), *id);
                found = true;
            }
            if let Some(u) = ex.units.get(name) {
                self.units.insert(name.clone(), u.clone());
                found = true;
            }
            if let Some(g) = ex.generic_fns.get(name) {
                self.generic_fns.insert(name.clone(), *g);
                found = true;
            }
            if let Some(g) = ex.generic_types.get(name) {
                self.generic_types.insert(name.clone(), *g);
                found = true;
            }
            if let Some(i) = ex.interfaces.get(name) {
                self.interface_index.insert(name.clone(), *i);
                found = true;
            }
            if !found {
                self.failed_imports.insert(name.clone());
                let names: Vec<String> = ex.public.iter().cloned().collect();
                let mut d = Diagnostic::new("E0602", n.span, format!("module `{module}` has no item `{name}`"));
                if let Some(s) = closest(name, names.iter()) {
                    d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), n.span, s);
                }
                self.err(d);
                continue;
            }
            // Where the item is declared, so its uses here lead there.
            let t = target.expect("exports come from a module");
            let theirs = self.scopes[t].as_ref().expect("an imported module is checked first");
            let (type_span, unit_span) = (theirs.type_spans.get(name).copied(), theirs.unit_spans.get(name).copied());
            if let Some(l) = type_span {
                self.type_spans.insert(name.clone(), l);
            }
            if let Some(l) = unit_span {
                self.unit_spans.insert(name.clone(), l);
            }
            let decl = match (ex.funcs.get(name), ex.generic_fns.get(name).or(ex.generic_types.get(name))) {
                (Some(&id), _) => Some(Loc { file: self.sigs[id].module, span: self.sigs[id].span }),
                (_, Some(&g)) => Some(self.generic_loc(g)),
                _ => ex
                    .interfaces
                    .get(name)
                    .map(|&i| self.interface_loc(i))
                    .or(type_span.or(unit_span))
                    .or(ex.consts.get(name).map(|c| c.loc)),
            };
            self.note_use(n.span, decl);
            if !ex.public.contains(name) {
                self.err(
                    Diagnostic::new("E0603", n.span, format!("`{name}` is private to module `{module}`"))
                        .note(format!("mark it pub in module `{module}` to import it"), None),
                );
            }
            self.imported.insert(name.clone(), (n.span, k));
        }
    }

    /// E0604 for each imported name the module never used, with its removal as the fix.
    fn unused_imports(&mut self, imports: &[&ast::ImportDecl]) {
        // A module's tests are left out when it is imported, so what they use looks unused then;
        // the file is checked on its own for this.
        if self.module != 0 {
            return;
        }
        let touched = self.touched.borrow().clone();
        for (k, imp) in imports.iter().enumerate() {
            for (i, n) in imp.names.iter().enumerate() {
                if self.imported.get(&n.name).map(|(_, item)| *item) != Some(k) || touched.contains(&n.name) {
                    continue;
                }
                let cut = if imp.names.len() == 1 {
                    // The whole import, with its line.
                    let after = &self.src.text[imp.span.end..];
                    let skip = after.len() - after.trim_start_matches([' ', '\t']).len();
                    let end = imp.span.end + skip + usize::from(after[skip..].starts_with('\n'));
                    Span::new(imp.span.start, end)
                } else if i + 1 < imp.names.len() {
                    Span::new(n.span.start, imp.names[i + 1].span.start)
                } else {
                    Span::new(imp.names[i - 1].span.end, n.span.end)
                };
                self.err(Diagnostic::new("E0604", n.span, format!("`{}` is imported but never used", n.name)).fix1(
                    FixKind::Mechanical,
                    "remove it from the import",
                    cut,
                    "",
                ));
            }
        }
    }

    /// E0607: a pub item may not show a private type of its module, or importers could hold
    /// values of a type they cannot name.
    fn check_public(
        &mut self,
        structs: &[&ast::StructDecl],
        enums: &[&ast::EnumDecl],
        fns: &[&ast::FnDecl],
        sig_base: usize,
        generics: &[usize],
    ) {
        let mut problems = Vec::new();
        for &g in generics.iter().filter(|g| self.generics[**g].public) {
            let (span, name) = match self.generics[g].kind {
                GenericKind::Fn(d) => (d.name.span, d.name.name.clone()),
                GenericKind::Struct(d) => (d.name.span, d.name.name.clone()),
                GenericKind::Enum(d) => (d.name.span, d.name.name.clone()),
            };
            for t in self.shown_types(g) {
                problems.extend(self.private_type(&t).map(|p| (span, name.clone(), p)));
            }
        }
        for d in structs.iter().filter(|d| d.public) {
            if let Some(&id) = self.struct_index.get(&d.name.name) {
                for (_, t) in &self.structs[id].fields {
                    problems.extend(self.private_type(t).map(|p| (d.name.span, d.name.name.clone(), p)));
                }
            }
        }
        for d in enums.iter().filter(|d| d.public) {
            if let Some(&id) = self.enum_index.get(&d.name.name) {
                for v in &self.enums[id].variants {
                    for (_, t) in &v.fields {
                        problems.extend(self.private_type(t).map(|p| (d.name.span, d.name.name.clone(), p)));
                    }
                }
            }
        }
        for (i, d) in fns.iter().enumerate().filter(|(_, d)| d.public) {
            let sig = &self.sigs[sig_base + i];
            for t in sig.params.iter().map(|(_, t)| t).chain([&sig.ret]) {
                problems.extend(self.private_type(t).map(|p| (d.name.span, d.name.name.clone(), p)));
            }
        }
        let mut seen = HashSet::new();
        for (span, item, ty) in problems {
            if seen.insert((item.clone(), ty.clone())) {
                self.err(
                    Diagnostic::new("E0607", span, format!("pub `{item}` uses the private type `{ty}`"))
                        .note(format!("mark `{ty}` pub too"), None),
                );
            }
        }
    }

    /// The first type in `t` that this module declares without pub.
    fn private_type(&self, t: &Ty) -> Option<String> {
        match t {
            Ty::Struct(id) if self.struct_owner[*id] == (self.module, false) => Some(self.structs[*id].name.clone()),
            Ty::Enum(id) => match self.enum_owner[*id] {
                Some((m, false)) if m == self.module => Some(self.enums[*id].name.clone()),
                Some(_) => None,
                None => self.enums[*id].variants.iter().flat_map(|v| &v.fields).find_map(|(_, t)| self.private_type(t)),
            },
            Ty::Array(e) => self.private_type(e),
            Ty::Map(_, v) => self.private_type(v),
            Ty::Fn(f) => f.params.iter().chain([&f.ret]).find_map(|t| self.private_type(t)),
            _ => None,
        }
    }

    fn check_new_name(&mut self, name: &ast::Ident, what: &str, seen: &mut HashMap<String, Span>) -> bool {
        if matches!(what, "unit" | "type" | "function" | "constant") {
            if let Some((prev, _)) = self.imported.get(&name.name) {
                let prev = *prev;
                self.err(
                    Diagnostic::new("E0201", name.span, format!("{what} `{}` is also imported", name.name))
                        .note("imported here", Some(prev)),
                );
                return false;
            }
        }
        if let Some(prev) = seen.get(&name.name) {
            let prev = *prev;
            self.err(
                Diagnostic::new("E0201", name.span, format!("{what} `{}` is declared twice", name.name))
                    .note("first declared here", Some(prev)),
            );
            return false;
        }
        seen.insert(name.name.clone(), name.span);
        true
    }

    fn declare_units(&mut self, decls: &[&ast::UnitDecl]) {
        let mut seen = HashMap::new();
        let mut aliases: HashMap<String, &ast::UnitExpr> = HashMap::new();
        for d in decls {
            if !self.check_new_name(&d.name, "unit", &mut seen) {
                continue;
            }
            self.unit_spans.insert(d.name.name.clone(), self.here(d.name.span));
            let text = self.text(d.span);
            self.def(d.name.span, d.name.name.clone(), "unit", text);
            match &d.def {
                None => {
                    self.units.insert(d.name.name.clone(), Unit::base(&d.name.name));
                }
                Some(e) => {
                    aliases.insert(d.name.name.clone(), e);
                }
            }
        }
        // Resolve aliases in source order; each may refer to others, cycles are errors.
        for d in decls {
            if d.def.is_some() && !self.units.contains_key(&d.name.name) {
                let mut visiting = HashSet::new();
                self.resolve_alias(&d.name.name, &aliases, &mut visiting);
            }
        }
    }

    fn resolve_alias(
        &mut self,
        name: &str,
        aliases: &HashMap<String, &ast::UnitExpr>,
        visiting: &mut HashSet<String>,
    ) -> Option<Unit> {
        if let Some(u) = self.units.get(name) {
            return Some(u.clone());
        }
        let expr = *aliases.get(name)?;
        if !visiting.insert(name.to_string()) {
            self.err(Diagnostic::new("E0201", expr.span, format!("unit `{name}` is defined in terms of itself")));
            return None;
        }
        let mut u = Unit::dimensionless();
        for (term, exp) in &expr.terms {
            let def = self.unit_spans.get(&term.name).copied();
            self.note_use(term.span, def);
            let t = if aliases.contains_key(&term.name) && !self.units.contains_key(&term.name) {
                self.resolve_alias(&term.name, aliases, visiting)?
            } else {
                self.lookup_unit(term)?
            };
            u = u.mul(&t.pow(*exp));
        }
        self.units.insert(name.to_string(), u.clone());
        Some(u)
    }

    fn lookup_unit(&mut self, name: &ast::Ident) -> Option<Unit> {
        self.touch(&name.name);
        if let Some(u) = self.units.get(&name.name) {
            let u = u.clone();
            let def = self.unit_spans.get(&name.name).copied();
            self.note_use(name.span, def);
            return Some(u);
        }
        if self.failed_imports.contains(&name.name) {
            return None;
        }
        let mut d = Diagnostic::new("E0200", name.span, format!("unknown unit `{}`", name.name));
        if let Some(s) = closest(&name.name, self.units.keys()) {
            d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), name.span, s);
        } else {
            d = d.note(format!("declare it at the top level: unit {};", name.name), None);
        }
        self.err(d);
        None
    }

    fn unit_expr(&mut self, e: &ast::UnitExpr) -> Option<Unit> {
        let mut u = Unit::dimensionless();
        let mut ok = true;
        for (term, exp) in &e.terms {
            match self.lookup_unit(term) {
                Some(t) => u = u.mul(&t.pow(*exp)),
                None => ok = false,
            }
        }
        ok.then_some(u)
    }

    fn resolve_type(&mut self, t: &ast::TypeExpr) -> Ty {
        let (name, unit, args) = match &t.kind {
            TypeKind::Array(elem) => {
                let e = self.resolve_type(elem);
                return if e == Ty::Error { Ty::Error } else { Ty::Array(Box::new(e)) };
            }
            TypeKind::Named { name, unit, args } => (name, unit, args),
            TypeKind::Fn { params, ret, uses, noalloc } => {
                let params: Vec<Ty> = params.iter().map(|p| self.resolve_type(p)).collect();
                let ret = ret.as_ref().map(|r| self.resolve_type(r)).unwrap_or(Ty::Void);
                let effects = self.effects(uses);
                if params.contains(&Ty::Error) || ret == Ty::Error {
                    return Ty::Error;
                }
                return Ty::Fn(Box::new(FnTy { params, ret, effects, noalloc: noalloc.is_some() }));
            }
        };
        let n = name.name.as_str();
        if let (Some(t), true, None) = (self.type_params.get(n), args.is_empty(), unit) {
            return t.clone();
        }
        self.touch(n);
        if let Some(&g) = self.generic_types.get(n) {
            let decl = self.generic_loc(g);
            self.note_use(name.span, Some(decl));
            return self.generic_type(g, t, args);
        }
        if n == "Option" || n == "Result" {
            let (want, shape) = if n == "Option" { (1, "Option[T]") } else { (2, "Result[T, E]") };
            if args.len() != want {
                self.err(Diagnostic::new("E0307", t.span, format!("{n} takes {want} type argument(s): {shape}")));
                return Ty::Error;
            }
            let tys: Vec<Ty> = args.iter().map(|a| self.resolve_type(a)).collect();
            if tys.contains(&Ty::Error) {
                return Ty::Error;
            }
            return if n == "Option" {
                self.option_of(tys[0].clone())
            } else {
                self.result_of(tys[0].clone(), tys[1].clone())
            };
        }
        if n == "Map" {
            if args.len() != 2 {
                self.err(Diagnostic::new("E0307", t.span, "Map takes 2 type arguments: Map[K, V]"));
                return Ty::Error;
            }
            let k = self.resolve_type(&args[0]);
            let v = self.resolve_type(&args[1]);
            if k == Ty::Error || v == Ty::Error || !self.map_key(&k, args[0].span) {
                return Ty::Error;
            }
            return Ty::Map(Box::new(k), Box::new(v));
        }
        if !args.is_empty() {
            let msg = if matches!(n, "i64" | "bool" | "str") {
                format!("only f64 carries units, not {n}")
            } else {
                format!("`{n}` takes no type arguments")
            };
            self.err(Diagnostic::new("E0307", t.span, msg));
            return Ty::Error;
        }
        match n {
            "i64" => Ty::I64,
            "bool" => Ty::Bool,
            "str" => Ty::Str,
            "bytes" => Ty::Bytes,
            "FileError" => Ty::Enum(self.file_error),
            "f64" => match unit {
                None => Ty::F64(Unit::dimensionless()),
                Some(u) => self.unit_expr(u).map(Ty::F64).unwrap_or(Ty::Error),
            },
            _ => {
                self.touch(n);
                if let Some(id) = self.struct_index.get(n).copied() {
                    let def = self.type_spans.get(n).copied();
                    self.note_use(name.span, def);
                    return Ty::Struct(id);
                }
                if let Some(id) = self.enum_index.get(n).copied() {
                    let def = self.type_spans.get(n).copied();
                    self.note_use(name.span, def);
                    return Ty::Enum(id);
                }
                let mut cands: Vec<String> = TYPE_NAMES.iter().map(|s| s.to_string()).collect();
                cands.extend(self.struct_index.keys().cloned());
                cands.extend(self.enum_index.keys().cloned());
                if self.failed_imports.contains(n) {
                    return Ty::Error;
                }
                let mut d = Diagnostic::new("E0307", name.span, format!("unknown type `{n}`"));
                if let Some(s) = closest(n, cands.iter()) {
                    d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), name.span, s);
                }
                self.err(d);
                Ty::Error
            }
        }
    }

    /// Map keys are i64 or str: the two types with an order every program agrees on.
    fn map_key(&mut self, k: &Ty, span: Span) -> bool {
        if matches!(k, Ty::I64 | Ty::Str) {
            return true;
        }
        if let Ty::Param(_) = k {
            if self.satisfies(k, Constraint::Key) {
                return true;
            }
            self.needs_constraint(k, Constraint::Key, span, "a map key");
            return false;
        }
        let t = self.show(k);
        self.err(
            Diagnostic::new("E0408", span, format!("map keys are i64 or str, not {t}"))
                .note("keys are kept in order; key by an i64 id or a str name", None),
        );
        false
    }

    fn builtin_enum(&mut self, key: String, name: String, variants: Vec<VariantDef>, args: Vec<Ty>) -> Ty {
        if let Some(id) = self.builtin_enums.get(&key) {
            return Ty::Enum(*id);
        }
        let id = self.enums.len();
        self.enums.push(EnumDef { name, variants });
        self.enum_owner.push(None);
        self.builtin_args.insert(id, args);
        self.builtin_enums.insert(key, id);
        Ty::Enum(id)
    }

    fn option_of(&mut self, t: Ty) -> Ty {
        let key = format!("Option[{t:?}]");
        let name = format!("Option[{}]", self.show(&t));
        let variants = vec![
            VariantDef { name: "Some".into(), fields: vec![("value".into(), t.clone())] },
            VariantDef { name: "None".into(), fields: Vec::new() },
        ];
        self.builtin_enum(key, name, variants, vec![t])
    }

    fn result_of(&mut self, t: Ty, e: Ty) -> Ty {
        let key = format!("Result[{t:?}, {e:?}]");
        let name = format!("Result[{}, {}]", self.show(&t), self.show(&e));
        let variants = vec![
            VariantDef { name: "Ok".into(), fields: vec![("value".into(), t.clone())] },
            VariantDef { name: "Err".into(), fields: vec![("error".into(), e.clone())] },
        ];
        self.builtin_enum(key, name, variants, vec![t, e])
    }

    /// "Option" or "Result" for instantiations of the built-in enums (FileError is a plain enum).
    fn builtin_kind(&self, id: EnumId) -> Option<&'static str> {
        let name = &self.enums[id].name;
        if self.enum_owner[id].is_some() || id == self.file_error {
            None
        } else if name.starts_with("Option[") {
            Some("Option")
        } else {
            Some("Result")
        }
    }

    fn declare_types(&mut self, struct_decls: &[&ast::StructDecl], enum_decls: &[&ast::EnumDecl]) {
        let mut seen = HashMap::new();
        let mut kept = Vec::new();
        for d in struct_decls {
            if TYPE_NAMES.contains(&d.name.name.as_str()) {
                self.err(Diagnostic::new("E0201", d.name.span, format!("`{}` is a built-in type", d.name.name)));
                continue;
            }
            if self.generic_types.contains_key(&d.name.name) {
                self.err(Diagnostic::new("E0201", d.name.span, format!("type `{}` is declared twice", d.name.name)));
                continue;
            }
            if self.check_new_name(&d.name, "type", &mut seen) {
                self.type_spans.insert(d.name.name.clone(), self.here(d.name.span));
                self.def(d.name.span, d.name.name.clone(), "struct", format!("struct {}", d.name.name));
                kept.push((self.structs.len(), *d));
                self.struct_index.insert(d.name.name.clone(), self.structs.len());
                self.structs.push(StructDef { name: d.name.name.clone(), fields: Vec::new() });
                self.struct_owner.push((self.module, d.public));
            }
        }
        let mut kept_enums = Vec::new();
        for d in enum_decls {
            if TYPE_NAMES.contains(&d.name.name.as_str()) {
                self.err(Diagnostic::new("E0201", d.name.span, format!("`{}` is a built-in type", d.name.name)));
                continue;
            }
            if self.generic_types.contains_key(&d.name.name) {
                self.err(Diagnostic::new("E0201", d.name.span, format!("type `{}` is declared twice", d.name.name)));
                continue;
            }
            if self.check_new_name(&d.name, "type", &mut seen) {
                self.type_spans.insert(d.name.name.clone(), self.here(d.name.span));
                self.def(d.name.span, d.name.name.clone(), "enum", format!("enum {}", d.name.name));
                self.enum_index.insert(d.name.name.clone(), self.enums.len());
                kept_enums.push((self.enums.len(), *d));
                self.enums.push(EnumDef { name: d.name.name.clone(), variants: Vec::new() });
                self.enum_owner.push(Some((self.module, d.public)));
            }
        }
        for &(id, d) in &kept {
            let mut fseen = HashMap::new();
            let mut fields = Vec::new();
            for (i, (f, t)) in d.fields.iter().enumerate() {
                self.check_new_name(f, "field", &mut fseen);
                let ty = self.resolve_type(t);
                self.field_spans.insert((id, i), self.here(f.span));
                let shown = self.show(&ty);
                self.def(f.span, format!("{}.{}", d.name.name, f.name), "field", shown);
                fields.push((f.name.clone(), ty));
            }
            self.structs[id].fields = fields;
        }
        for (id, d) in &kept_enums {
            let mut vseen = HashMap::new();
            let mut variants = Vec::new();
            for (vi, v) in d.variants.iter().enumerate() {
                self.check_new_name(&v.name, "variant", &mut vseen);
                self.variant_spans.insert((*id, vi), self.here(v.name.span));
                let vname = format!("{}.{}", d.name.name, v.name.name);
                let text = self.text(v.span);
                self.def(v.name.span, vname.clone(), "variant", text);
                let mut fseen = HashMap::new();
                let mut fields = Vec::new();
                for (fi, (f, t)) in v.fields.iter().enumerate() {
                    self.check_new_name(f, "field", &mut fseen);
                    let ty = self.resolve_type(t);
                    self.variant_field_spans.insert((*id, vi, fi), self.here(f.span));
                    let shown = self.show(&ty);
                    self.def(f.span, format!("{vname}.{}", f.name), "field", shown);
                    fields.push((f.name.clone(), ty));
                }
                variants.push(VariantDef { name: v.name.name.clone(), fields });
            }
            self.enums[*id].variants = variants;
        }
        // A type containing itself by value has no size; arrays are the way to nest.
        for &(id, d) in &kept {
            let me = Ty::Struct(id);
            if self.contains_by_value(&me, &me, &mut Vec::new()) {
                self.err(
                    Diagnostic::new("E0308", d.name.span, format!("struct `{}` contains itself", d.name.name))
                        .note("hold the inner values in an array instead: [T]", None),
                );
            }
        }
        for (id, d) in &kept_enums {
            let me = Ty::Enum(*id);
            if self.contains_by_value(&me, &me, &mut Vec::new()) {
                self.err(
                    Diagnostic::new("E0308", d.name.span, format!("enum `{}` contains itself", d.name.name))
                        .note("hold the inner values in an array instead: [T]", None),
                );
            }
        }
    }

    /// Types stored inline in a value of `ty` (array elements live elsewhere).
    fn inline_children(&self, ty: &Ty) -> Vec<Ty> {
        match ty {
            Ty::Struct(id) => self.structs[*id].fields.iter().map(|(_, t)| t.clone()).collect(),
            Ty::Enum(id) => {
                self.enums[*id].variants.iter().flat_map(|v| v.fields.iter().map(|(_, t)| t.clone())).collect()
            }
            _ => Vec::new(),
        }
    }

    fn contains_by_value(&self, outer: &Ty, target: &Ty, seen: &mut Vec<Ty>) -> bool {
        for c in self.inline_children(outer) {
            if c == *target {
                return true;
            }
            if !seen.contains(&c) {
                seen.push(c.clone());
                if self.contains_by_value(&c, target, seen) {
                    return true;
                }
            }
        }
        false
    }

    fn effects(&mut self, uses: &[ast::Ident]) -> Effects {
        let mut out = Effects::default();
        for u in uses {
            match Effect::from_name(&u.name) {
                Some(e) if out.has(e) => {
                    self.err(Diagnostic::new("E0201", u.span, format!("effect `{}` is listed twice", u.name)))
                }
                Some(e) => out.add(e),
                None => {
                    let mut d = Diagnostic::new(
                        "E0332",
                        u.span,
                        format!("unknown effect `{}`; the effects are io, fs, clock and random", u.name),
                    );
                    let names: Vec<String> = EFFECTS.iter().map(|e| e.to_string()).collect();
                    if u.name == "alloc" || u.name == "noalloc" {
                        d = d.note(
                            "allocation is not an effect: write `noalloc` after the signature to promise none",
                            None,
                        );
                    } else if let Some(s) = closest(&u.name, names.iter()) {
                        d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), u.span, s);
                    }
                    self.err(d);
                }
            }
        }
        out
    }

    /// Registers generic functions, structs and enums by name; their shapes come later.
    /// Returns the generic methods, to be recorded on their types (`register_method`) once the
    /// module's types are declared.
    fn declare_generics(&mut self, decls: &[GenericKind<'a>]) -> Vec<(&'a ast::FnDecl, usize)> {
        let mut seen_types = HashMap::new();
        let mut seen_fns = HashMap::new();
        let mut methods = Vec::new();
        for kind in decls {
            if let GenericKind::Fn(d) = *kind {
                if let Some(r) = &d.receiver {
                    let full = format!("{}.{}", r.ty.name, d.name.name);
                    let header = self.text(Span::new(d.span.start, d.body.span.start)).trim_end().to_string();
                    let tps = self.type_params(&d.type_params);
                    self.def(d.name.span, full.clone(), "method", header);
                    let g = self.generics.len();
                    self.add_generic(*kind, &ast::Ident { name: full, span: d.name.span }, tps, d.public);
                    methods.push((d, g));
                    continue;
                }
            }
            let (name, params, public, is_fn, header) = match *kind {
                GenericKind::Fn(d) => {
                    let header = self.text(Span::new(d.span.start, d.body.span.start)).trim_end().to_string();
                    (&d.name, &d.type_params, d.public, true, header)
                }
                GenericKind::Struct(d) => (&d.name, &d.type_params, d.public, false, format!("struct {}", d.name.name)),
                GenericKind::Enum(d) => (&d.name, &d.type_params, d.public, false, format!("enum {}", d.name.name)),
            };
            let what = if is_fn { "function" } else { "type" };
            let seen = if is_fn { &mut seen_fns } else { &mut seen_types };
            if !self.check_new_name(name, what, seen) {
                continue;
            }
            let tps = self.type_params(params);
            self.def(name.span, name.name.clone(), if is_fn { "fn" } else { "type" }, header);
            if !is_fn {
                self.type_spans.insert(name.name.clone(), self.here(name.span));
            }
            self.add_generic(*kind, name, tps, public);
        }
        methods
    }

    fn declare_fns(&mut self, decls: &[&ast::FnDecl]) {
        let mut seen = HashMap::new();
        for d in decls {
            if let Some(r) = &d.receiver {
                let params = d.params.iter().map(|p| (p.name.name.clone(), self.resolve_type(&p.ty))).collect();
                let ret = d.ret.as_ref().map(|t| self.resolve_type(t)).unwrap_or(Ty::Void);
                let effects = self.effects(&d.uses);
                let header = self.text(Span::new(d.span.start, d.body.span.start)).trim_end().to_string();
                self.def(d.name.span, format!("{}.{}", r.ty.name, d.name.name), "method", header);
                let id = self.sigs.len();
                self.sigs.push(FuncSig {
                    params,
                    ret,
                    effects,
                    noalloc: d.noalloc.is_some(),
                    noalloc_insert: noalloc_insert(d),
                    span: d.name.span,
                    module: self.module,
                });
                self.register_method(d, MethodFn::Fn(id));
                continue;
            }
            if BUILTINS.contains(&d.name.name.as_str()) || CONSTRUCTORS.contains(&d.name.name.as_str()) {
                self.err(Diagnostic::new("E0201", d.name.span, format!("`{}` is a built-in function", d.name.name)));
            }
            let mut fresh = self.check_new_name(&d.name, "function", &mut seen);
            if self.generic_fns.contains_key(&d.name.name) {
                self.err(Diagnostic::new(
                    "E0201",
                    d.name.span,
                    format!("function `{}` is declared twice", d.name.name),
                ));
                fresh = false;
            }
            let params = d.params.iter().map(|p| (p.name.name.clone(), self.resolve_type(&p.ty))).collect();
            let ret = d.ret.as_ref().map(|t| self.resolve_type(t)).unwrap_or(Ty::Void);
            let effects = self.effects(&d.uses);
            if fresh {
                self.func_index.insert(d.name.name.clone(), self.sigs.len());
            }
            let header = self.text(Span::new(d.span.start, d.body.span.start)).trim_end().to_string();
            self.def(d.name.span, d.name.name.clone(), "fn", header);
            // Duplicates still get a slot so FuncIds line up with fn_decls order.
            self.sigs.push(FuncSig {
                params,
                ret,
                effects,
                noalloc: d.noalloc.is_some(),
                noalloc_insert: noalloc_insert(d),
                span: d.name.span,
                module: self.module,
            });
        }
    }

    // ---------------- bodies ----------------

    /// `uses` lists the effects as written; `uses_span` is the whole clause.
    fn new_body(
        &self,
        ret: Ty,
        effects: Effects,
        uses: &[ast::Ident],
        uses_span: Option<Span>,
        uses_insert: Span,
        noalloc: Option<Span>,
    ) -> Body {
        Body {
            locals: Vec::new(),
            meta: Vec::new(),
            scopes: vec![HashMap::new()],
            declared: HashMap::new(),
            facts: HashMap::new(),
            ret,
            uses: effects,
            used: Effects::default(),
            uses_list: uses.to_vec(),
            uses_span,
            uses_insert,
            noalloc,
            loop_depth: 0,
            result_local: None,
            in_ensures: false,
            in_contract: false,
            name: String::new(),
            enclosing: None,
            captures: Vec::new(),
            owner: None,
        }
    }

    /// `body` (the block at `at`) with a marker first that counts its runs, except in a test or in
    /// a generic's abstract body, whose IR is dropped. An empty block is no branch: it has nothing
    /// to test (`None => {}` where no value is missing).
    /// Works out the module's constants, which can name each other in any order.
    fn declare_consts(&mut self, decls: &[&'a ast::ConstDecl]) {
        let mut seen = HashMap::new();
        let mut own: HashMap<&str, &'a ast::ConstDecl> = HashMap::new();
        for d in decls {
            let name = d.name.name.as_str();
            let taken = self.func_index.contains_key(name)
                || self.generic_fns.contains_key(name)
                || self.struct_index.contains_key(name)
                || self.enum_index.contains_key(name)
                || self.generic_types.contains_key(name)
                || BUILTINS.contains(&name)
                || CONSTRUCTORS.contains(&name);
            if taken {
                self.err(Diagnostic::new("E0201", d.name.span, format!("constant `{name}` has the name of a function or a type")));
                self.failed_imports.insert(name.to_string());
            } else if self.check_new_name(&d.name, "constant", &mut seen) {
                own.insert(name, d);
            }
        }
        for d in decls {
            if own.get(d.name.name.as_str()).is_some_and(|first| std::ptr::eq(*first, *d)) {
                self.const_value(d, &own, &mut Vec::new());
            }
        }
    }

    /// The value of constant `d`, worked out the first time it is asked for; `doing` holds those
    /// being worked out. A constant that fails counts as a failed import: its error is enough.
    fn const_value(
        &mut self,
        d: &'a ast::ConstDecl,
        own: &HashMap<&str, &'a ast::ConstDecl>,
        doing: &mut Vec<String>,
    ) -> Option<Expr> {
        let name = &d.name.name;
        if let Some(c) = self.consts.get(name) {
            return Some(c.value.clone());
        }
        if self.failed_imports.contains(name) {
            return None;
        }
        if doing.contains(name) {
            self.failed_imports.insert(name.clone());
            return self.not_const(d.name.span, format!("constant `{name}` depends on itself"));
        }
        doing.push(name.clone());
        let want = self.resolve_type(&d.ty);
        let value = self.const_fold(&d.value, own, doing);
        doing.pop();
        if !matches!(want, Ty::I64 | Ty::F64(_) | Ty::Bool | Ty::Str | Ty::Error) {
            let t = self.show(&want);
            self.failed_imports.insert(name.clone());
            return self.not_const(d.ty.span, format!("a constant is an i64, f64, bool or str, not {t}"));
        }
        let fits = value.as_ref().is_some_and(|x| {
            self.require_ty(&d.value, x, &want);
            x.ty == want
        });
        if !fits {
            self.failed_imports.insert(name.clone());
            return None;
        }
        let text = self.text(d.span);
        self.def(d.name.span, name.clone(), "const", text);
        let loc = self.here(d.name.span);
        self.consts.insert(name.clone(), Const { value: value.clone()?, loc });
        value
    }

    fn not_const(&mut self, span: Span, message: String) -> Option<Expr> {
        self.err(Diagnostic::new("E0351", span, message));
        None
    }

    /// The literal `e` comes to, as an expression every use can be.
    fn const_fold(
        &mut self,
        e: &ast::Expr,
        own: &HashMap<&str, &'a ast::ConstDecl>,
        doing: &mut Vec<String>,
    ) -> Option<Expr> {
        let lit = |kind, ty| Some(Expr { kind, ty });
        match &e.kind {
            AK::Int(v) => lit(ExprKind::Int(*v), Ty::I64),
            AK::Float(v, unit) => {
                let u = match unit {
                    None => Unit::dimensionless(),
                    Some(u) => self.unit_expr(u)?,
                };
                lit(ExprKind::Float(*v), Ty::F64(u))
            }
            AK::Bool(v) => lit(ExprKind::Bool(*v), Ty::Bool),
            AK::Str(s) => lit(ExprKind::Str(s.clone()), Ty::Str),
            AK::Paren(inner) => self.const_fold(inner, own, doing),
            AK::Name(id) => {
                self.touch(&id.name);
                self.used_consts.insert(id.name.clone());
                if let Some(d) = own.get(id.name.as_str()).copied() {
                    let def = self.here(d.name.span);
                    self.note_use(id.span, Some(def));
                    return self.const_value(d, own, doing);
                }
                if let Some(c) = self.consts.get(&id.name).cloned() {
                    self.note_use(id.span, Some(c.loc));
                    return Some(c.value);
                }
                if self.failed_imports.contains(&id.name) {
                    return None;
                }
                self.not_const(id.span, format!("`{}` is not a constant", id.name))
            }
            AK::Unary(op, x) => {
                let x = self.const_fold(x, own, doing)?;
                let ty = x.ty.clone();
                match (op, &x.kind) {
                    (UnOp::Neg, ExprKind::Int(v)) => match v.checked_neg() {
                        Some(v) => lit(ExprKind::Int(v), ty),
                        None => self.not_const(e.span, "this constant overflows i64".to_string()),
                    },
                    (UnOp::Neg, ExprKind::Float(v)) => lit(ExprKind::Float(-v), ty),
                    (UnOp::Not, ExprKind::Bool(v)) => lit(ExprKind::Bool(!v), ty),
                    (UnOp::BitNot, ExprKind::Int(v)) => lit(ExprKind::Int(!v), ty),
                    _ => self.not_const(e.span, "this operator does not apply to that value".to_string()),
                }
            }
            AK::Binary(op, l, r) => {
                let (a, b) = (self.const_fold(l, own, doing), self.const_fold(r, own, doing));
                let (a, b) = (a?, b?);
                match fold_binary(*op, &a, &b) {
                    Ok(x) => Some(x),
                    Err(why) => self.not_const(e.span, why),
                }
            }
            _ => self.not_const(e.span, "a constant is made of literals, other constants and operators".to_string()),
        }
    }

    fn marked(&mut self, b: &Body, at: Span, kind: BranchKind, mut body: Vec<Stmt>) -> Vec<Stmt> {
        if body.is_empty() {
            return body;
        }
        // A branch that panics says it should not run: a test could not pass through it anyway.
        if let Some(Stmt::Expr(Expr { kind: ExprKind::Builtin(Builtin::Panic { .. }, _), .. })) = body.last() {
            return body;
        }
        if let Some(id) = self.branch_id(b, at, kind) {
            body.insert(0, Stmt::Reached(id));
        }
        body
    }

    /// The id of the branch at `at`, whose runs count for the `unreached` lint.
    fn branch_id(&mut self, b: &Body, at: Span, kind: BranchKind) -> Option<BranchId> {
        let owner = b.owner?;
        if self.abstract_body {
            return None;
        }
        Some(*self.branch_ids.entry((self.module, at.start)).or_insert_with(|| {
            self.branches.push(Branch { module: self.module, at, owner, kind });
            self.branches.len() - 1
        }))
    }

    /// `let x: T = e else why { ... };`: the arm of the value binds `x`, the other leaves.
    fn let_else(&mut self, b: &mut Body, s: &ast::Stmt, other: &ast::LetElse) -> Option<Stmt> {
        let StmtKind::Let { mutable, kw_span, name, ty, init, .. } = &s.kind else { unreachable!("a let") };
        let t = self.resolve_type(ty);
        let e = self.expr(b, init);
        let meta = |span, kw_span, mutable| LocalMeta {
            span,
            kw_span,
            mutable,
            is_param: false,
            is_loop_var: false,
            used: false,
            assigned: false,
            rename_fix: None,
        };
        let inner = match &e.ty {
            Ty::Enum(id) if self.enums[*id].name.starts_with("Option[") || self.enums[*id].name.starts_with("Result[") => {
                Some(*id)
            }
            Ty::Error => None,
            other_ty => {
                let shown = self.show(other_ty);
                self.err(Diagnostic::new("E0352", init.span, format!("`let ... else` takes an Option or a Result, not {shown}")));
                None
            }
        };
        let Some(enum_id) = inner else {
            // Its error is enough: uses of the name say nothing more.
            self.failed_imports.insert(name.name.clone());
            return None;
        };
        let def = self.enums[enum_id].clone();
        let is_result = def.name.starts_with("Result[");
        let value_ty = def.variants[0].fields[0].1.clone();
        if value_ty != t && t != Ty::Error {
            let (holds, declared) = (self.show(&value_ty), self.show(&t));
            self.err(Diagnostic::new("E0300", init.span, format!("this holds {holds}, but `{}` is declared {declared}", name.name)));
        }
        b.scopes.push(HashMap::new());
        let mut bindings = Vec::new();
        if let Some(why) = &other.binding {
            if is_result {
                let err_ty = def.variants[RESULT_ERR].fields[0].1.clone();
                let m = meta(why.span, None, false);
                bindings.push((0, self.declare_local(b, why, err_ty, m)));
            } else {
                self.err(Diagnostic::new("E0352", why.span, "an Option's None holds nothing to name"));
            }
        }
        let (at, end) = (other.body.span, other.body.span.end);
        let body = self.block(b, &other.body);
        let leaves = |st: &[ast::Stmt]| always_returns(st) || st.iter().any(|x| matches!(x.kind, StmtKind::Break | StmtKind::Continue));
        if !leaves(&other.body.stmts) {
            self.err(Diagnostic::new(
                "E0352",
                at,
                "the `else` of a `let` must leave: return, break, continue, exit or panic",
            ));
        }
        let variant = if is_result { "Err" } else { "None" };
        let body = self.marked(b, at, BranchKind::Arm(variant.to_string()), body);
        self.close_scope(b, end);
        let held = self.hidden(b, e.ty.clone());
        let m = meta(name.span, Some(*kw_span), *mutable);
        let id = self.declare_local(b, name, t, m);
        let arms = vec![
            Arm { variant: 0, also: Vec::new(), bindings: vec![(0, id)], body: Vec::new() },
            Arm { variant: 1, also: Vec::new(), bindings, body },
        ];
        Some(Stmt::Match { held, scrutinee: e, arms })
    }

    fn declare_local(&mut self, b: &mut Body, name: &ast::Ident, ty: Ty, meta: LocalMeta) -> LocalId {
        if b.scopes.iter().any(|s| s.contains_key(&name.name)) {
            let prev = b.declared[&name.name];
            self.err(
                Diagnostic::new("E0202", name.span, format!("`{}` is already declared here", name.name))
                    .note("first declared here, and still in scope", Some(prev)),
            );
        } else if let Some((_, prev)) = b.enclosing.as_ref().and_then(|e| e.get(&name.name)) {
            let prev = *prev;
            self.err(
                Diagnostic::new(
                    "E0202",
                    name.span,
                    format!("`{}` is already declared in the function around this literal", name.name),
                )
                .note("first declared here", Some(prev)),
            );
        } else if self.consts.contains_key(&name.name) {
            self.err(Diagnostic::new("E0202", name.span, format!("`{}` is the name of a constant", name.name)));
        } else if self.func_index.contains_key(&name.name) || self.generic_fns.contains_key(&name.name) {
            self.err(Diagnostic::new("E0202", name.span, format!("`{}` is the name of a function", name.name)));
        } else if self.struct_index.contains_key(&name.name)
            || self.enum_index.contains_key(&name.name)
            || self.generic_types.contains_key(&name.name)
            || CONSTRUCTORS.contains(&name.name.as_str())
        {
            self.err(Diagnostic::new(
                "E0202",
                name.span,
                format!("`{}` is the name of a type or constructor", name.name),
            ));
        }
        b.declared.insert(name.name.clone(), name.span);
        let kind = if meta.is_param { "param" } else { "local" };
        let shown = self.show(&ty);
        let fact = self.def(name.span, name.name.clone(), kind, shown);
        let id = b.locals.len();
        if let Some(i) = fact {
            b.facts.insert(id, i);
        }
        b.locals.push(Local { name: name.name.clone(), ty });
        b.meta.push(meta);
        b.scopes.last_mut().unwrap().insert(name.name.clone(), id);
        id
    }

    /// A compiler-made local (match scrutinee, loop snapshot, `?` value). Never reported unused.
    fn hidden(&mut self, b: &mut Body, ty: Ty) -> LocalId {
        let id = b.locals.len();
        b.locals.push(Local { name: format!("${id}"), ty });
        b.meta.push(LocalMeta {
            span: Span::default(),
            kw_span: None,
            mutable: false,
            is_param: false,
            is_loop_var: false,
            used: true,
            assigned: false,
            rename_fix: None,
        });
        id
    }

    fn finish_locals(&mut self, b: &Body) {
        for (id, m) in b.meta.iter().enumerate() {
            let name = &b.locals[id].name;
            // A method may ignore `self` (an interface asks for the method, not for its use).
            if !m.used && !name.starts_with('_') && name != "self" {
                let mut d = Diagnostic::new("E0340", m.span, format!("`{name}` is never used"));
                if m.is_param {
                    d = d.note(
                        "rename the parameter to start with _ if it is unused on purpose (callers name it too)",
                        None,
                    );
                } else if let Some((span, text)) = &m.rename_fix {
                    d = d.fix1(FixKind::ChangesMeaning, "ignore the field with `_`", *span, text.clone());
                } else {
                    d = d.fix1(
                        FixKind::ChangesMeaning,
                        format!("rename to `_{name}` if unused on purpose"),
                        m.span,
                        format!("_{name}"),
                    );
                }
                self.err(d);
            } else if m.mutable && !m.assigned && !m.is_loop_var {
                let mut d = Diagnostic::new("E0341", m.span, format!("`{name}` is declared var but never reassigned"));
                if let Some(kw) = m.kw_span {
                    d = d.fix1(FixKind::Mechanical, "declare it with let", kw, "let");
                }
                self.err(d);
            }
        }
    }

    /// Every declared effect must be used (E0331); the fix takes it out of the clause.
    fn check_effect_usage(&mut self, b: &Body) {
        let Some(clause) = b.uses_span else { return };
        let list = &b.uses_list;
        for (i, u) in list.iter().enumerate() {
            let Some(e) = Effect::from_name(&u.name) else { continue };
            if b.used.has(e) || list[..i].iter().any(|p| p.name == u.name) {
                continue;
            }
            let span = if list.len() == 1 {
                Span::new(b.uses_insert.start, clause.end)
            } else if i + 1 < list.len() {
                Span::new(u.span.start, list[i + 1].span.start)
            } else {
                Span::new(list[i - 1].span.end, u.span.end)
            };
            let message = format!("declares uses {} but never {}", e.name(), e.doing());
            self.err(Diagnostic::new("E0331", u.span, message).fix1(
                FixKind::Mechanical,
                format!("remove `{}`", e.name()),
                span,
                "",
            ));
        }
    }

    /// Valid `allow LINT "reason"` lines as (lint, reason); `fn_start` is where the `fn` keyword begins.
    fn allows(&mut self, decls: &[ast::AllowDecl], fn_start: usize) -> Vec<(String, String)> {
        let mut allows: Vec<(String, String)> = Vec::new();
        for (i, a) in decls.iter().enumerate() {
            // Removing a line takes the space up to the next line's text with it.
            let line = Span::new(a.span.start, decls.get(i + 1).map_or(fn_start, |n| n.span.start));
            let name = &a.lint.name;
            if diag::lint(name).is_none() {
                let lints: Vec<String> = diag::LINTS.iter().map(|(n, _)| n.to_string()).collect();
                let mut d = Diagnostic::new("E0500", a.lint.span, format!("unknown lint `{name}`"));
                if let Some(s) = closest(name, lints.iter()) {
                    d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), a.lint.span, s);
                } else {
                    d = d.note(format!("the lints are: {}", lints.join(", ")), None);
                }
                self.err(d);
            } else if a.reason.trim().is_empty() {
                self.err(Diagnostic::new("E0501", a.reason_span, format!("`allow {name}` needs a reason")));
            } else if allows.iter().any(|(l, _)| l == name) {
                self.err(
                    Diagnostic::new("E0502", a.span, format!("`{name}` is already allowed for this function")).fix1(
                        FixKind::Mechanical,
                        "remove the duplicate allow",
                        line,
                        "",
                    ),
                );
            } else {
                allows.push((name.clone(), a.reason.clone()));
            }
        }
        allows
    }

    fn func(&mut self, d: &ast::FnDecl) -> (FuncId, Func) {
        let id = self.sigs.iter().position(|s| s.module == self.module && s.span == d.name.span).unwrap();
        let f = match &d.external {
            Some(x) => self.extern_func(d, x, id),
            None => self.func_at(d, id),
        };
        (id, f)
    }

    /// An extern function: only its signature, which must be one C can have.
    fn extern_func(&mut self, d: &ast::FnDecl, x: &ast::Extern, id: FuncId) -> Func {
        let ret = self.sigs[id].ret.clone();
        let effects = self.sigs[id].effects;
        let mut b = self.new_body(ret.clone(), effects, &d.uses, d.uses_span, d.uses_insert, d.noalloc);
        let crosses = |t: &Ty| matches!(t, Ty::I64 | Ty::F64(_) | Ty::Error);
        let mut params = Vec::new();
        for (i, p) in d.params.iter().enumerate() {
            let ty = self.sigs[id].params[i].1.clone();
            if !crosses(&ty) {
                let t = self.show(&ty);
                self.err(
                    Diagnostic::new("E0800", p.ty.span, format!("{t} cannot be passed to C"))
                        .note("extern functions take and return i64 and f64 (with any unit)", None),
                );
            }
            let meta = LocalMeta {
                span: p.name.span,
                kw_span: None,
                mutable: false,
                is_param: true,
                is_loop_var: false,
                // C reads it; Anvil cannot see that.
                used: true,
                assigned: false,
                rename_fix: None,
            };
            params.push(self.declare_local(&mut b, &p.name, ty, meta));
        }
        if !crosses(&ret) && ret != Ty::Void {
            let t = self.show(&ret);
            self.err(
                Diagnostic::new("E0800", d.ret.as_ref().unwrap().span, format!("C cannot return {t}"))
                    .note("extern functions take and return i64 and f64 (with any unit)", None),
            );
        }
        if let Some(io) = d.uses.iter().find(|u| u.name == "io") {
            self.err(
                Diagnostic::new("E0802", io.span, "an extern function cannot perform io")
                    .note("C writes past Anvil's output, which tests, goldens and tools capture; return the values and print them in Anvil", None),
            );
        }
        if let Some(fs) = d.uses.iter().find(|u| u.name == "fs") {
            self.err(
                Diagnostic::new("E0802", fs.span, "an extern function cannot read or write files")
                    .note("C would write to the disk where tests and goldens keep writes in memory; read and write the files in Anvil and pass the values", None),
            );
        }
        let name = &d.name.name;
        if name.starts_with("anvil_") || name == "main" {
            self.err(
                Diagnostic::new("E0803", d.name.span, format!("`{name}` is reserved for the Anvil runtime")).note(
                    "the runtime defines main and every symbol starting with anvil_; give the C function another name",
                    None,
                ),
            );
        }
        let file = match &x.source {
            None => None,
            Some((written, span)) if !written.ends_with(".c") => {
                self.err(Diagnostic::new("E0801", *span, format!("`{written}` is not a C file")).note(
                    "an extern names a .c file, relative to this file's directory, or nothing for the C runtime",
                    None,
                ));
                None
            }
            Some((written, span)) => match self.module_c.get(written).cloned() {
                Some(Ok(i)) => Some(i),
                Some(Err(why)) => {
                    self.err(
                        Diagnostic::new("E0801", *span, format!("cannot read C file `{written}`")).note(why, None),
                    );
                    None
                }
                None => {
                    let why = "no files can be read here; check the program as part of a project";
                    self.err(
                        Diagnostic::new("E0801", *span, format!("cannot read C file `{written}`")).note(why, None),
                    );
                    None
                }
            },
        };
        self.close_scope(&mut b, d.body.span.end);
        let allows = self.allows(&d.allows, d.span.start);
        Func {
            name: d.name.name.clone(),
            span: d.name.span,
            allows,
            module: self.module,
            instance: false,
            params,
            ret,
            effects,
            external: Some(ExternFn { symbol: d.name.name.clone(), file }),
            requires: Vec::new(),
            ensures: Vec::new(),
            steps: None,
            allocs: None,
            result_local: None,
            locals: b.locals,
            body: Vec::new(),
            captures: None,
        }
    }

    /// Checks the body of `d` against signature `id` (an instance's, for a generic function).
    fn func_at(&mut self, d: &ast::FnDecl, id: FuncId) -> Func {
        let ret = self.sigs[id].ret.clone();
        let effects = self.sigs[id].effects;
        let mut b = self.new_body(ret.clone(), effects, &d.uses, d.uses_span, d.uses_insert, d.noalloc);
        b.owner = Some(d.name.span);
        b.name = match &d.receiver {
            Some(r) => format!("{}.{}", r.ty.name, d.name.name),
            None => d.name.name.clone(),
        };
        let mut params = Vec::new();
        for (i, p) in d.params.iter().enumerate() {
            let ty = self.sigs[id].params[i].1.clone();
            let meta = LocalMeta {
                span: p.name.span,
                kw_span: None,
                mutable: false,
                is_param: true,
                is_loop_var: false,
                used: false,
                assigned: false,
                rename_fix: None,
            };
            params.push(self.declare_local(&mut b, &p.name, ty, meta));
        }
        b.in_contract = true;
        let mut requires = Vec::new();
        for e in &d.requires {
            let x = self.expect(&mut b, e, &Ty::Bool);
            let message = format!("requires failed at {}: {}", self.site(e.span), self.text(e.span));
            requires.push(Check { expr: x, message });
        }
        let mut ensures = Vec::new();
        let mut result_local = None;
        if !d.ensures.is_empty() && ret != Ty::Void {
            let id = b.locals.len();
            b.locals.push(Local { name: "result".into(), ty: ret.clone() });
            b.meta.push(LocalMeta {
                span: d.name.span,
                kw_span: None,
                mutable: false,
                is_param: false,
                is_loop_var: false,
                used: true,
                assigned: false,
                rename_fix: None,
            });
            result_local = Some(id);
        }
        b.result_local = result_local;
        b.in_ensures = true;
        for e in &d.ensures {
            let x = self.expect(&mut b, e, &Ty::Bool);
            let message = format!("ensures failed at {}: {}", self.site(e.span), self.text(e.span));
            ensures.push(Check { expr: x, message });
        }
        b.in_ensures = false;
        // The steps bound reads the parameters only: it is known before the call runs.
        let steps = d.steps.as_ref().map(|e| {
            let x = self.expect(&mut b, e, &Ty::I64);
            Check { expr: x, message: format!("its steps bound at {}: {}", self.site(e.span), self.text(e.span)) }
        });
        let allocs = d.allocs.as_ref().map(|e| {
            let x = self.expect(&mut b, e, &Ty::I64);
            Check { expr: x, message: format!("its allocs bound at {}: {}", self.site(e.span), self.text(e.span)) }
        });
        b.in_contract = false;
        let body = self.block(&mut b, &d.body);
        self.close_scope(&mut b, d.body.span.end);
        if ret != Ty::Void && ret != Ty::Error && !always_returns(&d.body.stmts) {
            self.err(
                Diagnostic::new(
                    "E0320",
                    d.body.span.at_end().to(Span::new(d.body.span.end - 1, d.body.span.end)),
                    format!("`{}` must return {} on every path", d.name.name, self.show(&ret)),
                )
                .note("the end of the function body can be reached without a return", None),
            );
        }
        self.finish_locals(&b);
        self.check_effect_usage(&b);
        let allows = self.allows(&d.allows, d.span.start);
        Func {
            name: d.name.name.clone(),
            span: d.name.span,
            allows,
            module: self.module,
            instance: false,
            params,
            ret,
            effects,
            external: None,
            requires,
            ensures,
            steps,
            allocs,
            result_local,
            locals: b.locals,
            body,
            captures: None,
        }
    }

    /// `fn(x: i64) -> i64 { ... }`: checked as a function of its own whose body may read the
    /// locals around it. Each one it reads is captured: copied into the closure when the literal
    /// is evaluated, and passed to the lifted function before its own parameters.
    fn lambda(&mut self, b: &mut Body, d: &ast::FnDecl) -> Expr {
        let params: Vec<(String, Ty)> =
            d.params.iter().map(|p| (p.name.name.clone(), self.resolve_type(&p.ty))).collect();
        let ret = d.ret.as_ref().map(|t| self.resolve_type(t)).unwrap_or(Ty::Void);
        let effects = self.effects(&d.uses);
        let ty = FnTy {
            params: params.iter().map(|(_, t)| t.clone()).collect(),
            ret: ret.clone(),
            effects,
            noalloc: d.noalloc.is_some(),
        };
        let id = self.sigs.len();
        self.sigs.push(FuncSig {
            params: params.clone(),
            ret: ret.clone(),
            effects,
            noalloc: d.noalloc.is_some(),
            noalloc_insert: noalloc_insert(d),
            span: d.name.span,
            module: self.module,
        });
        // Every local visible here, and whatever this body could itself capture.
        let mut names = b.enclosing.clone().unwrap_or_default();
        for scope in &b.scopes {
            for (n, &l) in scope {
                names.insert(n.clone(), (b.locals[l].ty.clone(), b.meta[l].span));
            }
        }
        let mut ib = self.new_body(ret.clone(), effects, &d.uses, d.uses_span, d.uses_insert, d.noalloc);
        let (line, col) = self.src.line_col(d.name.span.start);
        let module = &self.module_names[self.module];
        let owner = if module.is_empty() { b.name.clone() } else { format!("{module}.{}", b.name) };
        ib.name = format!("{owner}/fn@{line}:{col}");
        ib.enclosing = Some(names);
        ib.owner = b.owner;
        let mut own = Vec::new();
        for (p, (_, t)) in d.params.iter().zip(&params) {
            let meta = LocalMeta {
                span: p.name.span,
                kw_span: None,
                mutable: false,
                is_param: true,
                is_loop_var: false,
                used: false,
                assigned: false,
                rename_fix: None,
            };
            own.push(self.declare_local(&mut ib, &p.name, t.clone(), meta));
        }
        let body = self.block(&mut ib, &d.body);
        self.close_scope(&mut ib, d.body.span.end);
        if ret != Ty::Void && ret != Ty::Error && !always_returns(&d.body.stmts) {
            self.err(
                Diagnostic::new(
                    "E0320",
                    d.body.span.at_end().to(Span::new(d.body.span.end - 1, d.body.span.end)),
                    format!("this function literal must return {} on every path", self.show(&ret)),
                )
                .note("the end of its body can be reached without a return", None),
            );
        }
        self.finish_locals(&ib);
        self.check_effect_usage(&ib);
        // The captured values, read here (which may capture them from further out in turn).
        let mut captures = Vec::new();
        for &l in &ib.captures {
            let name = ast::Ident { name: ib.locals[l].name.clone(), span: d.name.span };
            let outer = self.lookup_local(b, &name).expect("a capture is visible around the literal");
            b.meta[outer].used = true;
            captures.push(Expr { kind: ExprKind::Local(outer), ty: b.locals[outer].ty.clone() });
        }
        if !captures.is_empty() {
            self.note_alloc(b, d.name.span, "a function literal that captures values");
        }
        let mut all_params = ib.captures.clone();
        all_params.extend(own);
        if !self.abstract_body {
            self.lifted.push((
                id,
                Func {
                    name: ib.name.clone(),
                    span: d.name.span,
                    allows: Vec::new(),
                    module: self.module,
                    instance: false,
                    params: all_params,
                    ret,
                    effects,
                    external: None,
                    requires: Vec::new(),
                    ensures: Vec::new(),
                    steps: None,
                    allocs: None,
                    result_local: None,
                    locals: ib.locals,
                    body,
                    captures: Some(captures.len()),
                },
            ));
        }
        Expr { kind: ExprKind::Closure { func: id, captures }, ty: Ty::Fn(Box::new(ty)) }
    }

    /// A declared function used as a value.
    fn fn_value(&mut self, id: &ast::Ident, fid: FuncId) -> Expr {
        let sig = &self.sigs[fid];
        let ty = FnTy {
            params: sig.params.iter().map(|(_, t)| t.clone()).collect(),
            ret: sig.ret.clone(),
            effects: sig.effects,
            noalloc: sig.noalloc,
        };
        let decl = Loc { file: sig.module, span: sig.span };
        self.note_use(id.span, Some(decl));
        if ty.params.contains(&Ty::Error) || ty.ret == Ty::Error {
            return Self::err_expr();
        }
        Expr { kind: ExprKind::FnRef(fid), ty: Ty::Fn(Box::new(ty)) }
    }

    /// `f(x, y)` where `f` is a local holding a function value: arguments in order, unnamed.
    fn call_value(&mut self, b: &mut Body, e: &ast::Expr, callee: &ast::Ident, l: LocalId, args: &[ast::Arg]) -> Expr {
        b.meta[l].used = true;
        self.note_use(callee.span, Some(self.here(b.meta[l].span)));
        let name = &callee.name;
        let f = match b.locals[l].ty.clone() {
            Ty::Fn(f) => *f,
            other => {
                if other != Ty::Error {
                    let t = self.show(&other);
                    self.err(
                        Diagnostic::new("E0413", callee.span, format!("`{name}` is {t}, not a function"))
                            .note("only a local holding a function value can be called by its name", None),
                    );
                }
                for a in args {
                    self.expr(b, &a.value);
                }
                return Self::err_expr();
            }
        };
        let mut named = false;
        for a in args {
            if let Some(n) = &a.name {
                named = true;
                self.err(
                    Diagnostic::new(
                        "E0304",
                        n.span,
                        format!(
                            "`{name}` is a function value, whose parameters have no names: pass arguments in order"
                        ),
                    )
                    .fix1(
                        FixKind::Mechanical,
                        "remove the name",
                        Span::new(n.span.start, a.value.span.start),
                        "",
                    ),
                );
            }
        }
        if args.len() != f.params.len() {
            let t = self.show(&Ty::Fn(Box::new(f.clone())));
            self.err(
                Diagnostic::new(
                    "E0303",
                    e.span,
                    format!("`{name}` takes {} argument(s), found {}", f.params.len(), args.len()),
                )
                .note(format!("its type: {t}"), None),
            );
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        }
        for eff in f.effects.iter() {
            self.note_effect(b, eff, callee.span, &format!("calling `{name}`"));
        }
        if !f.noalloc {
            self.note_alloc_call(b, callee.span, name, None);
        }
        let vals: Vec<Expr> = args.iter().zip(&f.params).map(|(a, t)| self.expect(b, &a.value, t)).collect();
        if named || vals.iter().any(|v| v.ty == Ty::Error) {
            return Self::err_expr();
        }
        let ty = f.ret.clone();
        let callee = Expr { kind: ExprKind::Local(l), ty: Ty::Fn(Box::new(f)) };
        Expr { kind: ExprKind::CallValue(Box::new(callee), vals, self.site(e.span)), ty }
    }

    /// Whether a value of `t` holds a function somewhere (functions cannot be compared or
    /// generated).
    fn has_fn(&self, t: &Ty) -> bool {
        self.has_fn_in(t, &mut Vec::new())
    }

    fn has_fn_in(&self, t: &Ty, seen: &mut Vec<Ty>) -> bool {
        match t {
            Ty::Fn(_) => true,
            Ty::Array(e) => self.has_fn_in(e, seen),
            Ty::Map(_, v) => self.has_fn_in(v, seen),
            Ty::Struct(id) if !seen.contains(t) => {
                seen.push(t.clone());
                self.structs[*id].fields.iter().any(|(_, f)| self.has_fn_in(f, seen))
            }
            Ty::Enum(id) if !seen.contains(t) => {
                seen.push(t.clone());
                self.enums[*id].variants.iter().flat_map(|v| &v.fields).any(|(_, f)| self.has_fn_in(f, seen))
            }
            _ => false,
        }
    }

    fn test(&mut self, t: &ast::TestDecl) -> Test {
        let effects = self.effects(&t.uses);
        let mut b = self.new_body(Ty::Void, effects, &t.uses, t.uses_span, t.uses_insert, None);
        b.name = format!("test {:?}", t.name);
        let mut inputs = Vec::new();
        for p in &t.inputs {
            let mut ty = self.resolve_type(&p.ty);
            if self.has_fn(&ty) {
                let shown = self.show(&ty);
                self.err(
                    Diagnostic::new("E0414", p.ty.span, format!("a property test cannot generate {shown}"))
                        .note("inputs are generated values; a function is not one: write a plain test that passes the functions to check", None),
                );
                ty = Ty::Error;
            }
            let meta = LocalMeta {
                span: p.name.span,
                kw_span: None,
                mutable: false,
                is_param: true,
                is_loop_var: false,
                used: false,
                assigned: false,
                rename_fix: None,
            };
            inputs.push(self.declare_local(&mut b, &p.name, ty, meta));
        }
        let body = self.block(&mut b, &t.body);
        self.close_scope(&mut b, t.body.span.end);
        self.finish_locals(&b);
        self.check_effect_usage(&b);
        Test { name: t.name.clone(), inputs, effects, locals: b.locals, body }
    }

    fn block(&mut self, b: &mut Body, block: &ast::Block) -> Vec<Stmt> {
        b.scopes.push(HashMap::new());
        let mut out = Vec::new();
        let mut terminated: Option<Span> = None;
        for s in &block.stmts {
            if let Some(t) = terminated {
                let last = block.stmts.last().unwrap().span;
                self.err(
                    Diagnostic::new("E0323", s.span, "unreachable code")
                        .note("control never gets past this statement", Some(t))
                        .fix1(FixKind::ChangesMeaning, "delete the unreachable statements", s.span.to(last), ""),
                );
                break;
            }
            if matches!(s.kind, StmtKind::Return(_) | StmtKind::Break | StmtKind::Continue) {
                terminated = Some(s.span);
            }
            if let Some(st) = self.stmt(b, s) {
                if self.keep_lines {
                    let line = self.src.line_col(s.span.start).0;
                    let at = format!("{}:{line}", self.src.name);
                    let lines = &mut self.lines;
                    let id = *self.line_ids.entry((self.module, line)).or_insert_with(|| {
                        lines.push(at);
                        lines.len() - 1
                    });
                    out.push(Stmt::Line(id));
                }
                out.push(st);
            }
        }
        self.close_scope(b, block.span.end);
        out
    }

    fn stmt(&mut self, b: &mut Body, s: &ast::Stmt) -> Option<Stmt> {
        Some(match &s.kind {
            StmtKind::Let { otherwise: Some(other), .. } => return self.let_else(b, s, other),
            StmtKind::Let { mutable, kw_span, name, ty, init, otherwise: None } => {
                let t = self.resolve_type(ty);
                let e = self.value(b, init, &t);
                let meta = LocalMeta {
                    span: name.span,
                    kw_span: Some(*kw_span),
                    mutable: *mutable,
                    is_param: false,
                    is_loop_var: false,
                    used: false,
                    assigned: false,
                    rename_fix: None,
                };
                let id = self.declare_local(b, name, t, meta);
                Stmt::Let(id, e)
            }
            StmtKind::Assign { target, value } => {
                let (local, path, ty) = self.place(b, target)?;
                if path.iter().any(|s| matches!(s, Step::Index { .. })) {
                    // The array is copied first if anything else shares it.
                    self.note_alloc(b, target.span, "assigning to an array element");
                }
                let v = self.value(b, value, &ty);
                Stmt::Assign(local, path, v, self.site(s.span))
            }
            StmtKind::Expr(e) => {
                let (AK::Call { .. } | AK::Method { .. }) = &e.kind else {
                    // Checked for its facts (completion after `p.` asks for p's type), but
                    // only this error is reported.
                    let errors = self.diags.len();
                    self.expr(b, e);
                    self.diags.truncate(errors);
                    self.err(Diagnostic::new(
                        "E0392",
                        e.span,
                        "this expression does nothing; only calls can be statements",
                    ));
                    return None;
                };
                let x = self.expr(b, e);
                if x.ty != Ty::Void && x.ty != Ty::Error {
                    let ty = self.show(&x.ty);
                    self.err(
                        Diagnostic::new("E0392", e.span, format!("the {ty} returned by this call is dropped")).fix1(
                            FixKind::ChangesMeaning,
                            "bind it to a name",
                            e.span.at_start(),
                            format!("let _unused: {ty} = "),
                        ),
                    );
                }
                Stmt::Expr(x)
            }
            StmtKind::If { cond, then, else_ } => {
                let c = self.condition(b, cond);
                let t = self.block(b, then);
                let t = self.marked(b, then.span, BranchKind::Then, t);
                // An `else if` is not a branch of its own: its blocks are.
                let e = match else_.as_deref() {
                    None => Vec::new(),
                    Some(Else::Block(blk)) => {
                        let e = self.block(b, blk);
                        self.marked(b, blk.span, BranchKind::Else, e)
                    }
                    Some(Else::If(st)) => self.stmt(b, st).into_iter().collect(),
                };
                Stmt::If(c, t, e)
            }
            StmtKind::While { cond, body } => {
                let c = self.condition(b, cond);
                b.loop_depth += 1;
                let at = body.span;
                let body = self.block(b, body);
                let body = self.marked(b, at, BranchKind::Loop, body);
                b.loop_depth -= 1;
                Stmt::While(c, body)
            }
            StmtKind::For { var, start, end, body } => {
                let s0 = self.expect(b, start, &Ty::I64);
                let e0 = self.expect(b, end, &Ty::I64);
                b.scopes.push(HashMap::new());
                let meta = LocalMeta {
                    span: var.span,
                    kw_span: None,
                    mutable: false,
                    is_param: false,
                    is_loop_var: true,
                    used: false,
                    assigned: false,
                    rename_fix: None,
                };
                let id = self.declare_local(b, var, Ty::I64, meta);
                b.loop_depth += 1;
                let (at, end) = (body.span, body.span.end);
                let body = self.block(b, body);
                let body = self.marked(b, at, BranchKind::Loop, body);
                b.loop_depth -= 1;
                self.close_scope(b, end);
                Stmt::For(id, s0, e0, body)
            }
            StmtKind::ForEach { var, iter, body } => {
                let it = self.expr(b, iter);
                let elem = match &it.ty {
                    Ty::Array(t) => (**t).clone(),
                    Ty::Error => Ty::Error,
                    other => {
                        let t = self.show(other);
                        let mut d = Diagnostic::new(
                            "E0300",
                            iter.span,
                            format!("for loops run over an array or a range start..end, not {t}"),
                        );
                        if let Ty::Map(..) = other {
                            d = d.note("loop over its keys or values: for k in m.keys() { }", None);
                        }
                        if *other == Ty::Bytes {
                            d = d.note("loop by index: for i in 0..b.len() { }, or over b.to_array()", None);
                        }
                        if *other == Ty::Str {
                            d = d.note("loop over its characters: for c in s.chars() { }", None);
                        }
                        self.err(d);
                        Ty::Error
                    }
                };
                let held = self.hidden(b, it.ty.clone());
                b.scopes.push(HashMap::new());
                let meta = LocalMeta {
                    span: var.span,
                    kw_span: None,
                    mutable: false,
                    is_param: false,
                    is_loop_var: true,
                    used: false,
                    assigned: false,
                    rename_fix: None,
                };
                let id = self.declare_local(b, var, elem, meta);
                b.loop_depth += 1;
                let (at, end) = (body.span, body.span.end);
                let body = self.block(b, body);
                let body = self.marked(b, at, BranchKind::Loop, body);
                b.loop_depth -= 1;
                self.close_scope(b, end);
                Stmt::ForEach { var: id, held, iter: it, body }
            }
            StmtKind::Match { scrutinee, arms, span_end } => return self.match_stmt(b, scrutinee, arms, *span_end),
            StmtKind::Return(v) => {
                let ret = b.ret.clone();
                match (v, &ret) {
                    (None, Ty::Void) => Stmt::Return(None),
                    (Some(e), Ty::Void) => {
                        self.err(
                            Diagnostic::new(
                                "E0321",
                                e.span,
                                "this function has no return type, so return takes no value",
                            )
                            .fix1(
                                FixKind::ChangesMeaning,
                                "drop the value",
                                Span::new(s.span.start + 6, e.span.end),
                                "",
                            ),
                        );
                        None?
                    }
                    (None, _) => {
                        let t = self.show(&ret);
                        self.err(Diagnostic::new("E0321", s.span, format!("this function must return a {t}")));
                        None?
                    }
                    (Some(e), _) => Stmt::Return(Some(self.value(b, e, &ret))),
                }
            }
            StmtKind::Break | StmtKind::Continue => {
                if b.loop_depth == 0 {
                    self.err(Diagnostic::new("E0370", s.span, "break and continue only work inside a loop"));
                    return None;
                }
                if matches!(s.kind, StmtKind::Break) {
                    Stmt::Break
                } else {
                    Stmt::Continue
                }
            }
        })
    }

    fn match_stmt(&mut self, b: &mut Body, scrutinee: &ast::Expr, arms: &[ast::Arm], span_end: Span) -> Option<Stmt> {
        let x = self.expr(b, scrutinee);
        let id = match &x.ty {
            Ty::Enum(id) => *id,
            Ty::Error => return None,
            other => {
                let t = self.show(other);
                self.err(
                    Diagnostic::new("E0403", scrutinee.span, format!("match needs an enum value, found {t}"))
                        .note("use if and else for i64, f64, bool and str", None),
                );
                for arm in arms {
                    self.unchecked_arm(b, arm);
                }
                return None;
            }
        };
        let variants = self.enums[id].variants.clone();
        let ename = self.enums[id].name.clone();
        let mut seen: HashMap<usize, Span> = HashMap::new();
        let mut unknown_arm = false;
        let mut out = Vec::new();
        for arm in arms {
            let Some(vi) = variants.iter().position(|v| v.name == arm.variant.name) else {
                unknown_arm = true;
                let names: Vec<String> = variants.iter().map(|v| v.name.clone()).collect();
                let mut d = Diagnostic::new(
                    "E0200",
                    arm.variant.span,
                    format!("`{ename}` has no variant `{}`", arm.variant.name),
                )
                .note(format!("variants: {}", names.join(", ")), None);
                if let Some(s) = closest(&arm.variant.name, names.iter()) {
                    d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), arm.variant.span, s);
                }
                self.err(d);
                self.unchecked_arm(b, arm);
                continue;
            };
            if let Some(prev) = seen.insert(vi, arm.variant.span) {
                self.err(
                    Diagnostic::new("E0401", arm.variant.span, format!("`{}` is matched twice", arm.variant.name))
                        .note("first matched here", Some(prev)),
                );
                self.unchecked_arm(b, arm);
                continue;
            }
            let def = self.variant_spans.get(&(id, vi)).copied();
            self.note_use(arm.variant.span, def);
            let mut also = Vec::new();
            for other in &arm.also {
                let Some(oi) = variants.iter().position(|v| v.name == other.name) else {
                    unknown_arm = true;
                    let names: Vec<String> = variants.iter().map(|v| v.name.clone()).collect();
                    self.err(
                        Diagnostic::new("E0200", other.span, format!("`{ename}` has no variant `{}`", other.name))
                            .note(format!("variants: {}", names.join(", ")), None),
                    );
                    continue;
                };
                if let Some(prev) = seen.insert(oi, other.span) {
                    self.err(
                        Diagnostic::new("E0401", other.span, format!("`{}` is matched twice", other.name))
                            .note("first matched here", Some(prev)),
                    );
                    continue;
                }
                let def = self.variant_spans.get(&(id, oi)).copied();
                self.note_use(other.span, def);
                also.push(oi);
            }
            b.scopes.push(HashMap::new());
            let bindings = if arm.also.is_empty() {
                self.pattern(b, arm, (id, vi), &variants[vi])
            } else {
                if arm.fields.is_some() {
                    self.err(
                        Diagnostic::new("E0402", arm.fields_span, "an arm with several variants binds no fields")
                            .note("name the variants alone: Red | Green => { ... }", None),
                    );
                }
                Vec::new()
            };
            let body = self.block(b, &arm.body);
            let mut shown = arm.variant.name.clone();
            for other in &arm.also {
                shown = format!("{shown} | {}", other.name);
            }
            let body = self.marked(b, arm.body.span, BranchKind::Arm(shown), body);
            self.close_scope(b, arm.body.span.end);
            out.push(Arm { variant: vi, also, bindings, body });
        }
        let missing: Vec<&VariantDef> =
            variants.iter().enumerate().filter(|(i, _)| !seen.contains_key(i)).map(|(_, v)| v).collect();
        if !missing.is_empty() && !unknown_arm {
            let names: Vec<&str> = missing.iter().map(|v| v.name.as_str()).collect();
            let arm_text: Vec<String> = missing
                .iter()
                .map(|v| {
                    if v.fields.is_empty() {
                        format!("{} => {{}}", v.name)
                    } else {
                        let fs: Vec<String> = v.fields.iter().map(|(f, _)| format!("{f}: _")).collect();
                        format!("{}({}) => {{}}", v.name, fs.join(", "))
                    }
                })
                .collect();
            // Insert before the closing brace, one arm per line when the brace starts its line.
            let line_start = self.src.text[..span_end.start].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let before = &self.src.text[line_start..span_end.start];
            let text = if before.trim().is_empty() {
                arm_text.iter().map(|a| format!("{INDENT}{a}\n{before}")).collect::<String>()
            } else {
                arm_text.iter().map(|a| format!("{a} ")).collect::<String>()
            };
            self.err(
                Diagnostic::new(
                    "E0400",
                    scrutinee.span,
                    format!("match on `{ename}` does not handle {}", names.join(", ")),
                )
                .note("every variant needs an arm; there is no catch-all, but one arm can name several: A | B => { ... }", None)
                .fix1(
                    FixKind::ChangesMeaning,
                    "add empty arms for the missing variants",
                    span_end.at_start(),
                    text,
                ),
            );
        }
        let held = self.hidden(b, x.ty.clone());
        Some(Stmt::Match { held, scrutinee: x, arms: out })
    }

    /// Checks the body of an arm whose pattern cannot be typed, so its own errors are still
    /// found; its bindings get the error type and are never reported unused.
    fn unchecked_arm(&mut self, b: &mut Body, arm: &ast::Arm) {
        b.scopes.push(HashMap::new());
        for pf in arm.fields.iter().flatten() {
            let name = pf.binding.clone().unwrap_or_else(|| pf.field.clone());
            if name.name != "_" {
                self.error_binding(b, &name);
            }
        }
        self.block(b, &arm.body);
        self.close_scope(b, arm.body.span.end);
    }

    fn error_binding(&mut self, b: &mut Body, name: &ast::Ident) {
        let meta = LocalMeta {
            span: name.span,
            kw_span: None,
            mutable: false,
            is_param: false,
            is_loop_var: false,
            used: true,
            assigned: false,
            rename_fix: None,
        };
        self.declare_local(b, name, Ty::Error, meta);
    }

    /// Checks an arm's field list against the variant and declares its bindings.
    fn pattern(&mut self, b: &mut Body, arm: &ast::Arm, at: (EnumId, usize), v: &VariantDef) -> Vec<(usize, LocalId)> {
        let vname = &arm.variant.name;
        let fields = match (&arm.fields, v.fields.is_empty()) {
            (None, true) => return Vec::new(),
            (Some(fs), true) => {
                let mut d = Diagnostic::new("E0402", arm.fields_span, format!("`{vname}` has no fields"));
                if fs.is_empty() {
                    d = d.fix1(FixKind::Mechanical, "remove the parentheses", arm.fields_span, "");
                }
                self.err(d);
                return Vec::new();
            }
            // Named alone, it binds nothing, as in an arm with several variants.
            (None, false) => {
                return Vec::new();
            }
            (Some(fs), false) => fs,
        };
        let decl: Vec<&str> = v.fields.iter().map(|(f, _)| f.as_str()).collect();
        // The only field takes any name, as its constructor takes the value unnamed.
        let only: Vec<ast::PatField>;
        let fields: &[ast::PatField] = match (fields.as_slice(), decl.as_slice()) {
            ([pf], [d]) if pf.binding.is_none() && pf.field.name != *d => {
                let field = ast::Ident { name: d.to_string(), span: pf.field.span };
                only = vec![ast::PatField { field, binding: Some(pf.field.clone()), span: pf.span }];
                &only
            }
            _ => fields,
        };
        let mut ok = true;
        let mut given: HashMap<&str, usize> = HashMap::new();
        for (i, pf) in fields.iter().enumerate() {
            if !decl.contains(&pf.field.name.as_str()) {
                ok = false;
                let owned: Vec<String> = decl.iter().map(|s| s.to_string()).collect();
                let mut d =
                    Diagnostic::new("E0402", pf.field.span, format!("`{vname}` has no field `{}`", pf.field.name))
                        .note(format!("fields: {}", decl.join(", ")), None);
                if let Some(s) = closest(&pf.field.name, owned.iter()) {
                    d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), pf.field.span, s);
                }
                self.err(d);
            } else if given.insert(pf.field.name.as_str(), i).is_some() {
                ok = false;
                self.err(Diagnostic::new("E0402", pf.field.span, format!("field `{}` is listed twice", pf.field.name)));
            }
        }
        if !ok {
            for pf in fields {
                let name = pf.binding.clone().unwrap_or_else(|| pf.field.clone());
                if name.name != "_" {
                    self.error_binding(b, &name);
                }
            }
            return Vec::new();
        }
        let missing: Vec<&str> = decl.iter().filter(|f| !given.contains_key(**f)).copied().collect();
        if !missing.is_empty() {
            // Rewrite the whole list in declaration order, ignoring the missing fields.
            let text: Vec<String> = decl
                .iter()
                .map(|d| match given.get(d) {
                    Some(i) => self.text(fields[*i].span),
                    None => format!("{d}: _"),
                })
                .collect();
            let inner = Span::new(arm.fields_span.start + 1, arm.fields_span.end - 1);
            self.err(
                Diagnostic::new(
                    "E0402",
                    arm.fields_span,
                    format!("pattern for `{vname}` leaves out {}", missing.join(", ")),
                )
                .note("list every field; write `field: _` to ignore one", None)
                .fix1(FixKind::ChangesMeaning, "ignore the missing fields", inner, text.join(", ")),
            );
            ok = false;
        }
        let in_order = fields.iter().zip(&decl).all(|(pf, d)| pf.field.name == *d);
        if ok && !in_order {
            let text: Vec<String> = decl.iter().map(|d| self.text(fields[given[d]].span)).collect();
            let inner = Span::new(fields[0].span.start, fields.last().unwrap().span.end);
            self.err(
                Diagnostic::new("E0402", arm.fields_span, format!("fields of `{vname}` must be in declaration order"))
                    .note(format!("declared order: {}", decl.join(", ")), None)
                    .fix1(FixKind::Mechanical, "reorder the fields", inner, text.join(", ")),
            );
            ok = false;
        }
        let mut bindings = Vec::new();
        for pf in fields {
            let name = pf.binding.clone().unwrap_or_else(|| pf.field.clone());
            if name.name == "_" {
                continue;
            }
            if !ok {
                self.error_binding(b, &name);
                continue;
            }
            let i = given[pf.field.name.as_str()];
            let def = self.variant_field_spans.get(&(at.0, at.1, i)).copied();
            self.note_use(pf.field.span, def);
            let rename_fix = match &pf.binding {
                None => Some((pf.field.span.at_end(), ": _".to_string())),
                Some(n) => Some((n.span, "_".to_string())),
            };
            let meta = LocalMeta {
                span: name.span,
                kw_span: None,
                mutable: false,
                is_param: false,
                is_loop_var: false,
                used: false,
                assigned: false,
                rename_fix,
            };
            let id = self.declare_local(b, &name, v.fields[i].1.clone(), meta);
            bindings.push((i, id));
        }
        bindings
    }

    /// The right side of let, assignment or return: the only places `?` may appear.
    fn value(&mut self, b: &mut Body, e: &ast::Expr, want: &Ty) -> Expr {
        if let AK::Try(inner) = &e.kind {
            let x = self.try_expr(b, e, inner);
            self.require_ty(e, &x, want);
            return x;
        }
        self.expect(b, e, want)
    }

    fn try_expr(&mut self, b: &mut Body, e: &ast::Expr, inner: &ast::Expr) -> Expr {
        let x = self.expr(b, inner);
        let id = match &x.ty {
            Ty::Error => return Self::err_expr(),
            Ty::Enum(id) if self.builtin_kind(*id).is_some() => *id,
            other => {
                let t = self.show(other);
                self.err(Diagnostic::new("E0405", e.span, format!("`?` needs an Option or a Result, found {t}")));
                return Self::err_expr();
            }
        };
        let kind = self.builtin_kind(id);
        let ret = b.ret.clone();
        if ret == Ty::Error {
            return Self::err_expr();
        }
        let fits = match &ret {
            Ty::Enum(rid) if self.builtin_kind(*rid) == kind => {
                kind == Some("Option")
                    || self.enums[*rid].variants[RESULT_ERR].fields[0].1
                        == self.enums[id].variants[RESULT_ERR].fields[0].1
            }
            _ => false,
        };
        if !fits {
            let rs = self.show(&ret);
            let msg = if kind == Some("Option") {
                format!("`?` on an Option returns None early, so the function must return an Option, not {rs}")
            } else {
                let es = self.show(&self.enums[id].variants[RESULT_ERR].fields[0].1.clone());
                format!("`?` on a Result returns its Err early, so the function must return Result[_, {es}], not {rs}")
            };
            self.err(Diagnostic::new("E0405", e.span, msg));
            return Self::err_expr();
        }
        let Ty::Enum(ret_enum) = ret else { unreachable!() };
        let payload = self.enums[id].variants[0].fields[0].1.clone();
        let held = self.hidden(b, x.ty.clone());
        Expr { kind: ExprKind::Try { held, inner: Box::new(x), ret_enum }, ty: payload }
    }

    fn condition(&mut self, b: &mut Body, cond: &ast::Expr) -> Expr {
        let c = self.expr(b, cond);
        if c.ty != Ty::Bool && c.ty != Ty::Error {
            let mut d = Diagnostic::new("E0380", cond.span, format!("condition is {}, not bool", self.show(&c.ty)));
            if c.ty == Ty::I64 {
                d = d.fix1(FixKind::ChangesMeaning, "compare with zero", cond.span.at_end(), " != 0");
            }
            self.err(d);
        }
        c
    }

    /// Resolves an assignment target to (local, field path, type).
    fn place(&mut self, b: &mut Body, target: &ast::Expr) -> Option<(LocalId, Vec<Step>, Ty)> {
        match &target.kind {
            AK::Name(id) => {
                let Some(local) = self.lookup_local(b, id) else {
                    self.unknown_name(b, id);
                    return None;
                };
                self.note_use(id.span, Some(self.here(b.meta[local].span)));
                if b.captures.contains(&local) {
                    self.err(
                        Diagnostic::new(
                            "E0410",
                            target.span,
                            format!("`{}` is captured by this function literal, so it cannot be assigned", id.name),
                        )
                        .note(
                            "a function literal gets a copy of each value it reads; return the new value instead",
                            None,
                        ),
                    );
                    return None;
                }
                let m = &mut b.meta[local];
                m.assigned = true;
                if !m.mutable {
                    let mut d = Diagnostic::new(
                        "E0310",
                        target.span,
                        format!("`{}` cannot be assigned: it is not var", id.name),
                    );
                    if m.is_param {
                        d = d.note("parameters are immutable; copy into a var first", None);
                    } else if m.is_loop_var {
                        d = d.note("loop variables are immutable", None);
                    } else if let Some(kw) = m.kw_span {
                        d = d.note("declared here", Some(m.span)).fix1(
                            FixKind::ChangesMeaning,
                            "declare it with var",
                            kw,
                            "var",
                        );
                    }
                    self.err(d);
                }
                Some((local, Vec::new(), b.locals[local].ty.clone()))
            }
            AK::Field(base, field) => {
                let (local, mut path, ty) = self.place(b, base)?;
                let Ty::Struct(sid) = ty else {
                    if ty != Ty::Error {
                        let t = self.show(&ty);
                        self.err(Diagnostic::new("E0306", field.span, format!("{t} has no fields")));
                    }
                    return None;
                };
                let idx = self.field_index(sid, field)?;
                path.push(Step::Field(idx));
                Some((local, path, self.structs[sid].fields[idx].1.clone()))
            }
            AK::Index(base, index) => {
                let (local, mut path, ty) = self.place(b, base)?;
                let i = self.expect(b, index, &Ty::I64);
                match ty {
                    Ty::Array(elem) => {
                        path.push(Step::Index { index: i, site: self.site(target.span) });
                        Some((local, path, *elem))
                    }
                    Ty::Error => None,
                    Ty::Bytes => {
                        self.err(
                            Diagnostic::new("E0310", target.span, "bytes cannot be changed in place")
                                .note("make new bytes: from an array with bytes_of, or by joining slices with +", None),
                        );
                        None
                    }
                    other => {
                        let t = self.show(&other);
                        self.err(Diagnostic::new("E0302", target.span, format!("only arrays can be indexed, not {t}")));
                        None
                    }
                }
            }
            AK::Paren(inner) => self.place(b, inner),
            _ => {
                self.err(Diagnostic::new(
                    "E0311",
                    target.span,
                    "only variables, their fields and array elements can be assigned",
                ));
                None
            }
        }
    }

    fn field_index(&mut self, sid: StructId, field: &ast::Ident) -> Option<usize> {
        let fields = &self.structs[sid].fields;
        if let Some(i) = fields.iter().position(|(n, _)| *n == field.name) {
            let def = self.field_spans.get(&(sid, i)).copied();
            self.note_use(field.span, def);
            return Some(i);
        }
        let names: Vec<String> = fields.iter().map(|(n, _)| n.clone()).collect();
        let sname = self.structs[sid].name.clone();
        let mut d = Diagnostic::new("E0306", field.span, format!("struct `{sname}` has no field `{}`", field.name))
            .note(format!("fields: {}", names.join(", ")), None);
        if let Some(s) = closest(&field.name, names.iter()) {
            d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), field.span, s);
        }
        self.err(d);
        None
    }

    /// The local `id` names: in this body's scopes or, in a function literal, one of the
    /// functions around it, which is then captured (once, the first time it is read).
    fn lookup_local(&self, b: &mut Body, id: &ast::Ident) -> Option<LocalId> {
        if let Some(l) = b.scopes.iter().rev().find_map(|s| s.get(&id.name).copied()) {
            return Some(l);
        }
        let (ty, span) = b.enclosing.as_ref()?.get(&id.name)?.clone();
        let l = b.locals.len();
        b.locals.push(Local { name: id.name.clone(), ty });
        b.meta.push(LocalMeta {
            span,
            kw_span: None,
            mutable: false,
            is_param: false,
            is_loop_var: false,
            used: true,
            assigned: false,
            rename_fix: None,
        });
        b.scopes[0].insert(id.name.clone(), l);
        b.captures.push(l);
        Some(l)
    }

    fn unknown_name(&mut self, b: &mut Body, id: &ast::Ident) {
        if self.failed_imports.contains(&id.name) {
            return;
        }
        let mut d = Diagnostic::new("E0200", id.span, format!("unknown name `{}`", id.name));
        if self.consts.contains_key(&id.name) {
            d = d.note(format!("`{}` is a constant, which cannot be assigned", id.name), None);
        } else if self.func_index.contains_key(&id.name) || BUILTINS.contains(&id.name.as_str()) {
            d = d.note(format!("`{}` is a function; call it with ( )", id.name), None);
        } else if self.enum_index.contains_key(&id.name) {
            d = d.note(format!("`{0}` is an enum; name a variant: {0}.Variant", id.name), None);
        } else {
            let visible: Vec<String> = b.scopes.iter().flat_map(|s| s.keys().cloned()).collect();
            if let Some(s) = closest(&id.name, visible.iter()) {
                // The suggested variable was probably meant to be read here; don't also call it unused.
                if let Some(l) = b.scopes.iter().rev().find_map(|sc| sc.get(&s).copied()) {
                    b.meta[l].used = true;
                }
                d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), id.span, s);
            } else if b.declared.contains_key(&id.name) {
                d = d.note("it is declared in a block that has already ended", b.declared.get(&id.name).copied());
            }
        }
        self.err(d);
    }

    // ---------------- expressions ----------------

    /// Checks `e` against an expected type, reporting mismatches with fixes.
    fn expect(&mut self, b: &mut Body, e: &ast::Expr, want: &Ty) -> Expr {
        let x = self.expr_hint(b, e, Some(want));
        self.require_ty(e, &x, want);
        x
    }

    fn require_ty(&mut self, e: &ast::Expr, x: &Expr, want: &Ty) {
        if x.ty == *want || x.ty == Ty::Error || *want == Ty::Error {
            return;
        }
        // A function that does less than a type allows still fits it.
        if let (Ty::Fn(have), Ty::Fn(w)) = (&x.ty, want) {
            if have.fits(w) {
                return;
            }
        }
        let got_s = self.show(&x.ty);
        let want_s = self.show(want);
        let d = match (&x.ty, want) {
            (Ty::Fn(have), Ty::Fn(w)) if have.params == w.params && have.ret == w.ret => {
                let extra: Vec<&str> = have.effects.iter().filter(|x| !w.effects.has(*x)).map(Effect::name).collect();
                let why = if extra.is_empty() {
                    "the type promises noalloc, and this function does not".to_string()
                } else {
                    format!("this function uses {}, which the type does not allow", extra.join(", "))
                };
                Diagnostic::new("E0300", e.span, format!("expected {want_s}, found {got_s}")).note(why, None)
            }
            (Ty::F64(_), Ty::F64(u)) => {
                let mut d = Diagnostic::new("E0301", e.span, format!("expected {want_s}, found {got_s}"));
                if let AK::Float(_, None) = &e.kind {
                    d = d.fix1(
                        FixKind::Mechanical,
                        format!("give the literal the unit {u}"),
                        e.span.at_end(),
                        format!("[{u}]"),
                    );
                }
                d
            }
            (Ty::I64, Ty::F64(u)) => {
                let mut d = Diagnostic::new("E0300", e.span, format!("expected {want_s}, found i64"));
                if let AK::Int(v) = &e.kind {
                    let unit = if u.is_dimensionless() { String::new() } else { format!("[{u}]") };
                    d = d.fix1(FixKind::Mechanical, "write a float literal", e.span, format!("{v}.0{unit}"));
                } else if u.is_dimensionless() {
                    d = d.fix(
                        FixKind::ChangesMeaning,
                        "convert with to_f64",
                        vec![
                            Edit { span: e.span.at_start(), text: "to_f64(".into() },
                            Edit { span: e.span.at_end(), text: ")".into() },
                        ],
                    );
                }
                d
            }
            (Ty::F64(u), Ty::I64) => {
                let mut d = Diagnostic::new("E0300", e.span, format!("expected i64, found {got_s}"));
                if u.is_dimensionless() {
                    d = d.fix(
                        FixKind::ChangesMeaning,
                        "truncate with to_i64",
                        vec![
                            Edit { span: e.span.at_start(), text: "to_i64(".into() },
                            Edit { span: e.span.at_end(), text: ")".into() },
                        ],
                    );
                }
                d
            }
            _ => Diagnostic::new("E0300", e.span, format!("expected {want_s}, found {got_s}")),
        };
        self.err(d);
    }

    fn err_expr() -> Expr {
        Expr { kind: ExprKind::Bool(false), ty: Ty::Error }
    }

    fn expr(&mut self, b: &mut Body, e: &ast::Expr) -> Expr {
        self.expr_hint(b, e, None)
    }

    /// `hint` is the expected type where one is known; it types `None`, `Ok`, `Err` and `[]`.
    fn expr_hint(&mut self, b: &mut Body, e: &ast::Expr, hint: Option<&Ty>) -> Expr {
        let x = self.expr_kind(b, e, hint);
        if x.ty != Ty::Error && self.module == 0 && self.quiet == 0 {
            let shown = self.show(&x.ty);
            self.facts.exprs.push((e.span, shown));
        }
        x
    }

    fn expr_kind(&mut self, b: &mut Body, e: &ast::Expr, hint: Option<&Ty>) -> Expr {
        let (kind, ty) = match &e.kind {
            AK::Int(v) => (ExprKind::Int(*v), Ty::I64),
            AK::Float(v, unit) => {
                let u = match unit {
                    None => Some(Unit::dimensionless()),
                    Some(u) => self.unit_expr(u),
                };
                match u {
                    Some(u) => (ExprKind::Float(*v), Ty::F64(u)),
                    None => return Self::err_expr(),
                }
            }
            AK::Bool(v) => (ExprKind::Bool(*v), Ty::Bool),
            AK::Str(s) => (ExprKind::Str(s.clone()), Ty::Str),
            AK::Paren(inner) => return self.expr_hint(b, inner, hint),
            AK::If(c, yes, no) => {
                let cond = self.expect(b, c, &Ty::Bool);
                let then = self.expr_hint(b, yes, hint);
                let otherwise = self.expect(b, no, &then.ty);
                if then.ty == Ty::Void {
                    self.err(Diagnostic::new("E0300", yes.span, "an `if` that gives a value needs one from each branch"));
                }
                let then_branch = self.branch_id(b, yes.span, BranchKind::Then);
                let else_branch = self.branch_id(b, no.span, BranchKind::Else);
                let ty = then.ty.clone();
                let kind = ExprKind::Cond {
                    cond: Box::new(cond),
                    then: Box::new(then),
                    otherwise: Box::new(otherwise),
                    then_branch,
                    else_branch,
                };
                (kind, ty)
            }
            AK::Name(id) if id.name == "None" => return self.builtin_ctor(b, e, id, None, hint),
            AK::Name(id) => match self.lookup_local(b, id) {
                Some(l) => {
                    b.meta[l].used = true;
                    self.note_use(id.span, Some(self.here(b.meta[l].span)));
                    (ExprKind::Local(l), b.locals[l].ty.clone())
                }
                None => {
                    self.touch(&id.name);
                    if let Some(c) = self.consts.get(&id.name).cloned() {
                        self.used_consts.insert(id.name.clone());
                        self.note_use(id.span, Some(c.loc));
                        return c.value;
                    }
                    if let Some(&fid) = self.func_index.get(&id.name) {
                        return self.fn_value(id, fid);
                    }
                    if self.generic_fns.contains_key(&id.name) || BUILTINS.contains(&id.name.as_str()) {
                        let what =
                            if BUILTINS.contains(&id.name.as_str()) { "a built-in" } else { "a generic function" };
                        self.err(
                            Diagnostic::new("E0412", id.span, format!("`{}` is {what}, which is not a value", id.name))
                                .note(
                                    format!(
                                        "pass a function literal that calls it, such as fn(x: i64) -> i64 {{ return {}(x); }}",
                                        id.name
                                    ),
                                    None,
                                ),
                        );
                        return Self::err_expr();
                    }
                    self.unknown_name(b, id);
                    return Self::err_expr();
                }
            },
            AK::Lambda(d) => return self.lambda(b, d),
            AK::Result => {
                if !b.in_ensures {
                    self.err(Diagnostic::new("E0322", e.span, "`result` is only valid in ensures clauses"));
                    return Self::err_expr();
                }
                match b.result_local {
                    Some(l) => (ExprKind::Local(l), b.locals[l].ty.clone()),
                    None => {
                        self.err(Diagnostic::new(
                            "E0322",
                            e.span,
                            "this function returns nothing, so there is no result",
                        ));
                        return Self::err_expr();
                    }
                }
            }
            AK::Field(base, field) if self.generic_enum(b, base).is_some() => {
                let g = self.generic_enum(b, base).unwrap();
                let what = format!("`{}.{}`", self.generics[g].name, field.name);
                return match self.infer_instance(b, g, hint, Vec::new(), e.span, &what) {
                    Some(id) => self.variant_ctor(b, e, (id, base.span), field, None),
                    None => Self::err_expr(),
                };
            }
            AK::Field(base, field) if self.enum_name(b, base).is_some() => {
                let eid = self.enum_name(b, base).unwrap();
                return self.variant_ctor(b, e, (eid, base.span), field, None);
            }
            AK::Field(base, field) => {
                let x = self.expr(b, base);
                match x.ty.clone() {
                    Ty::Struct(sid) => match self.field_index(sid, field) {
                        Some(i) => {
                            let ty = self.structs[sid].fields[i].1.clone();
                            (ExprKind::Field(Box::new(x), i), ty)
                        }
                        None => return Self::err_expr(),
                    },
                    Ty::Error => return Self::err_expr(),
                    other => {
                        let t = self.show(&other);
                        let mut d = Diagnostic::new("E0306", field.span, format!("{t} has no fields"));
                        if let Ty::Enum(_) = other {
                            d = d.note("read a variant's fields with match", None);
                        }
                        self.err(d);
                        return Self::err_expr();
                    }
                }
            }
            AK::Method { receiver, method, args } => return self.method(b, e, receiver, method, args, hint),
            AK::Index(base, index) => {
                let x = self.expr(b, base);
                let i = self.expect(b, index, &Ty::I64);
                match x.ty.clone() {
                    Ty::Array(elem) => {
                        let site = self.site(e.span);
                        let plain = !mutates(&i);
                        (ExprKind::Index { base: Box::new(x), index: Box::new(i), site, plain }, *elem)
                    }
                    Ty::Bytes => {
                        let site = self.site(e.span);
                        let plain = !mutates(&i);
                        (ExprKind::Index { base: Box::new(x), index: Box::new(i), site, plain }, Ty::I64)
                    }
                    Ty::Error => return Self::err_expr(),
                    other => {
                        let t = self.show(&other);
                        let mut d =
                            Diagnostic::new("E0302", e.span, format!("only arrays and bytes can be indexed, not {t}"));
                        if other == Ty::Str {
                            d = d.note("strings are not indexed by position in Anvil", None);
                        }
                        self.err(d);
                        return Self::err_expr();
                    }
                }
            }
            AK::Slice(base, from, to) => {
                let x = self.expr(b, base);
                let f = self.expect(b, from, &Ty::I64);
                let t = self.expect(b, to, &Ty::I64);
                match x.ty.clone() {
                    ty @ (Ty::Array(_) | Ty::Str | Ty::Bytes) => {
                        self.note_alloc(b, e.span, "a slice");
                        let site = self.site(e.span);
                        (ExprKind::Slice { base: Box::new(x), from: Box::new(f), to: Box::new(t), site }, ty)
                    }
                    Ty::Error => return Self::err_expr(),
                    other => {
                        let t = self.show(&other);
                        self.err(Diagnostic::new(
                            "E0302",
                            e.span,
                            format!("only arrays, str and bytes can be sliced, not {t}"),
                        ));
                        return Self::err_expr();
                    }
                }
            }
            AK::MapLit(entries) => {
                // The expected type was already reported as wrong; one error is enough.
                if hint == Some(&Ty::Error) {
                    for (ke, ve) in entries {
                        self.expr(b, ke);
                        self.expr(b, ve);
                    }
                    return Self::err_expr();
                }
                let (k, v, hinted) = match (hint, entries.first()) {
                    (Some(Ty::Map(k, v)), _) => ((**k).clone(), (**v).clone(), true),
                    (_, Some((k0, v0))) => {
                        let k = self.expr(b, k0).ty;
                        let v = self.expr(b, v0).ty;
                        (k, v, false)
                    }
                    (_, None) => {
                        self.err(
                            Diagnostic::new(
                                "E0404",
                                e.span,
                                "the key and value types of `[:]` cannot be inferred here",
                            )
                            .note("state the type: let m: Map[str, i64] = [:];", None),
                        );
                        return Self::err_expr();
                    }
                };
                if k == Ty::Error || v == Ty::Error || (!hinted && !self.map_key(&k, entries[0].0.span)) {
                    for (ke, ve) in entries.iter().skip(if hinted { 0 } else { 1 }) {
                        self.expr(b, ke);
                        self.expr(b, ve);
                    }
                    return Self::err_expr();
                }
                let vals: Vec<(Expr, Expr)> =
                    entries.iter().map(|(ke, ve)| (self.expect(b, ke, &k), self.expect(b, ve, &v))).collect();
                let duplicate = duplicate_key_message(&self.site(e.span));
                self.note_alloc(b, e.span, "a map literal");
                (ExprKind::MapLit { entries: vals, duplicate }, Ty::Map(Box::new(k), Box::new(v)))
            }
            AK::Array(elems) if hint == Some(&Ty::Error) => {
                for x in elems {
                    self.expr(b, x);
                }
                return Self::err_expr();
            }
            AK::Array(elems) => {
                let elem_hint = match hint {
                    Some(Ty::Array(t)) => Some((**t).clone()),
                    _ => None,
                };
                let elem = match (elem_hint, elems.first()) {
                    (Some(t), _) => t,
                    (None, Some(first)) => {
                        let x = self.expr(b, first);
                        x.ty
                    }
                    (None, None) => {
                        self.err(
                            Diagnostic::new("E0404", e.span, "the element type of `[]` cannot be inferred here")
                                .note("state the type: let xs: [i64] = [];", None),
                        );
                        return Self::err_expr();
                    }
                };
                if elem == Ty::Error {
                    for x in elems {
                        self.expr(b, x);
                    }
                    return Self::err_expr();
                }
                let vals: Vec<Expr> = elems.iter().map(|x| self.expect(b, x, &elem)).collect();
                self.note_alloc(b, e.span, "an array literal");
                (ExprKind::ArrayLit(vals), Ty::Array(Box::new(elem)))
            }
            AK::Interp(parts) => {
                let mut out = Vec::new();
                let mut bad = false;
                for part in parts {
                    match part {
                        InterpPart::Lit(text) => out.push(Expr { kind: ExprKind::Str(text.clone()), ty: Ty::Str }),
                        InterpPart::Expr(pe) => {
                            let x = self.expr(b, pe);
                            if x.ty == Ty::Error {
                                bad = true;
                            } else if matches!(x.ty, Ty::Param(_)) && !self.satisfies(&x.ty, Constraint::Text) {
                                bad = true;
                                self.needs_constraint(&x.ty, Constraint::Text, pe.span, "interpolating it");
                            } else if !printable(&x.ty) && !matches!(x.ty, Ty::Param(_)) {
                                bad = true;
                                let t = self.show(&x.ty);
                                self.err(Diagnostic::new(
                                    "E0407",
                                    pe.span,
                                    format!("{t} has no text form; only i64, f64, bool and str can be interpolated"),
                                ));
                            }
                            out.push(x);
                        }
                    }
                }
                if bad {
                    return Self::err_expr();
                }
                self.note_alloc(b, e.span, "interpolation");
                (ExprKind::Interp(out), Ty::Str)
            }
            AK::Try(inner) => {
                self.expr(b, inner);
                self.err(
                    Diagnostic::new(
                        "E0405",
                        e.span,
                        "`?` can only apply to the whole value of let, var, an assignment or return",
                    )
                    .note("bind the result first: let v: T = f(x)?;", None),
                );
                return Self::err_expr();
            }
            AK::StructLit { name, fields } => return self.struct_lit(b, e, name, fields, hint),
            AK::Call { callee, args } if CONSTRUCTORS.contains(&callee.name.as_str()) => {
                return self.builtin_ctor(b, e, callee, Some(args), hint)
            }
            AK::Call { callee, args } => return self.call(b, e, callee, args),
            AK::Unary(op, inner) => {
                let x = self.expr(b, inner);
                match (op, &x.ty) {
                    (_, Ty::Error) => return Self::err_expr(),
                    (UnOp::Neg, Ty::I64) => {
                        let overflow = overflow_message("-", &self.site(e.span));
                        (ExprKind::INeg { e: Box::new(x), overflow }, Ty::I64)
                    }
                    (UnOp::Neg, Ty::F64(u)) => {
                        let u = u.clone();
                        (ExprKind::FNeg(Box::new(x)), Ty::F64(u))
                    }
                    (UnOp::Not, Ty::Bool) => (ExprKind::Not(Box::new(x)), Ty::Bool),
                    (UnOp::BitNot, Ty::I64) => (ExprKind::BitNot(Box::new(x)), Ty::I64),
                    (op, t) => {
                        let o = match op {
                            UnOp::Neg => "-",
                            UnOp::Not => "!",
                            UnOp::BitNot => "~",
                        };
                        let t = self.show(t);
                        self.err(Diagnostic::new("E0302", e.span, format!("`{o}` is not defined for {t}")));
                        return Self::err_expr();
                    }
                }
            }
            AK::Binary(op, l, r) => return self.binary(b, e, *op, l, r),
        };
        Expr { kind, ty }
    }

    fn binary(&mut self, b: &mut Body, e: &ast::Expr, op: BinOp, l: &ast::Expr, r: &ast::Expr) -> Expr {
        let x = self.expr(b, l);
        // `opt == None`, `xs == []`: the left side gives the right its type.
        let y = if matches!(op, BinOp::Eq | BinOp::Ne)
            && matches!(x.ty, Ty::Enum(_) | Ty::Array(_) | Ty::Map(..) | Ty::Struct(_))
        {
            let hint = x.ty.clone();
            self.expr_hint(b, r, Some(&hint))
        } else {
            self.expr(b, r)
        };
        if x.ty == Ty::Error || y.ty == Ty::Error {
            return Self::err_expr();
        }
        if op == BinOp::Caret {
            let (ls, rs) = (self.text(l.span), self.text(r.span));
            let wrap =
                |s: String| if s.chars().all(|c| c.is_alphanumeric() || c == '_') { s } else { format!("({s})") };
            self.err(
                Diagnostic::new("E0393", e.span, "`^` is not an operator in Anvil expressions")
                    .note("`^` is a power in units (m/s^2); exclusive or is a.xor(b), and a square is x * x", None)
                    .fix1(FixKind::ChangesMeaning, "exclusive or", e.span, format!("{}.xor({rs})", wrap(ls))),
            );
            return Self::err_expr();
        }
        if matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::Shl | BinOp::Shr) {
            if x.ty == Ty::I64 && y.ty == Ty::I64 {
                let bop = match op {
                    BinOp::BitAnd => BitOp::And,
                    BinOp::BitOr => BitOp::Or,
                    BinOp::Shl => BitOp::Shl,
                    _ => BitOp::Shr,
                };
                let site = self.site(e.span);
                return Expr { kind: ExprKind::Bits { op: bop, l: Box::new(x), r: Box::new(y), site }, ty: Ty::I64 };
            }
            let (lt, rt) = (self.show(&x.ty), self.show(&y.ty));
            let mut d = Diagnostic::new("E0302", e.span, format!("`{}` is not defined for {lt} and {rt}", op.text()))
                .note("bit operators take i64", None);
            if x.ty == Ty::Bool && y.ty == Ty::Bool && matches!(op, BinOp::BitAnd | BinOp::BitOr) {
                let logical = if op == BinOp::BitAnd { "&&" } else { "||" };
                let text = format!("{} {logical} {}", self.text(l.span), self.text(r.span));
                d = d.fix1(
                    FixKind::ChangesMeaning,
                    format!("logical {}: `{logical}`", if logical == "&&" { "and" } else { "or" }),
                    e.span,
                    text,
                );
            }
            self.err(d);
            return Self::err_expr();
        }
        if x.ty == Ty::Bytes && y.ty == Ty::Bytes {
            match op {
                BinOp::Add => {
                    self.note_alloc(b, e.span, "joining bytes");
                    let site = self.site(e.span);
                    return Expr { kind: ExprKind::StrConcat(Box::new(x), Box::new(y), site), ty: Ty::Bytes };
                }
                BinOp::Eq | BinOp::Ne => {
                    return Expr { kind: ExprKind::Cmp(cmp_op(op), Box::new(x), Box::new(y)), ty: Ty::Bool };
                }
                _ => {
                    self.err(
                        Diagnostic::new("E0302", e.span, format!("`{}` is not defined for bytes", op.text()))
                            .note("bytes have + (joining), == and !=", None),
                    );
                    return Self::err_expr();
                }
            }
        }
        if matches!(op, BinOp::Eq | BinOp::Ne) && (self.has_fn(&x.ty) || self.has_fn(&y.ty)) {
            let t = self.show(if self.has_fn(&x.ty) { &x.ty } else { &y.ty });
            let what = if matches!(x.ty, Ty::Fn(_)) { String::new() } else { format!(", and {t} holds one") };
            self.err(Diagnostic::new("E0411", e.span, format!("functions cannot be compared{what}")).note(
                "two functions that compute the same thing are not equal values; compare what they return",
                None,
            ));
            return Self::err_expr();
        }
        let site = self.site(e.span);
        let bx = |v: Expr| Box::new(v);
        let (lt, rt) = (x.ty.clone(), y.ty.clone());
        if lt == Ty::Str && rt == Ty::Str {
            if op == BinOp::Add {
                self.note_alloc(b, e.span, "joining strings");
                return Expr { kind: ExprKind::StrConcat(bx(x), bx(y), site), ty: Ty::Str };
            }
            if op.is_comparison() {
                return Expr { kind: ExprKind::Cmp(cmp_op(op), bx(x), bx(y)), ty: Ty::Bool };
            }
        }
        match op {
            BinOp::And | BinOp::Or => {
                if lt == Ty::Bool && rt == Ty::Bool {
                    let kind = if op == BinOp::And { ExprKind::And(bx(x), bx(y)) } else { ExprKind::Or(bx(x), bx(y)) };
                    return Expr { kind, ty: Ty::Bool };
                }
            }
            _ if lt == Ty::I64 && rt == Ty::I64 => {
                let iop = match op {
                    BinOp::Add => Some(IOp::Add),
                    BinOp::Sub => Some(IOp::Sub),
                    BinOp::Mul => Some(IOp::Mul),
                    BinOp::Div => Some(IOp::Div),
                    BinOp::Rem => Some(IOp::Rem),
                    _ => None,
                };
                if let Some(iop) = iop {
                    let kind = ExprKind::IArith {
                        op: iop,
                        l: bx(x),
                        r: bx(y),
                        overflow: overflow_message(op.text(), &site),
                        div_zero: div_zero_message(&site),
                    };
                    return Expr { kind, ty: Ty::I64 };
                }
                return Expr { kind: ExprKind::Cmp(cmp_op(op), bx(x), bx(y)), ty: Ty::Bool };
            }
            _ => {
                if let (Ty::F64(a), Ty::F64(c)) = (&lt, &rt) {
                    let (a, c) = (a.clone(), c.clone());
                    match op {
                        BinOp::Mul => {
                            return Expr { kind: ExprKind::FArith(FOp::Mul, bx(x), bx(y)), ty: Ty::F64(a.mul(&c)) }
                        }
                        BinOp::Div => {
                            return Expr { kind: ExprKind::FArith(FOp::Div, bx(x), bx(y)), ty: Ty::F64(a.div(&c)) }
                        }
                        BinOp::Rem => {}
                        _ if a != c => {
                            let d = Diagnostic::new(
                                "E0301",
                                e.span,
                                format!(
                                    "`{}` needs matching units, found {} and {}",
                                    op.text(),
                                    self.show(&lt),
                                    self.show(&rt)
                                ),
                            );
                            self.err(self.unit_fix(d, l, r, &a, &c));
                            return Self::err_expr();
                        }
                        BinOp::Add => return Expr { kind: ExprKind::FArith(FOp::Add, bx(x), bx(y)), ty: Ty::F64(a) },
                        BinOp::Sub => return Expr { kind: ExprKind::FArith(FOp::Sub, bx(x), bx(y)), ty: Ty::F64(a) },
                        _ => return Expr { kind: ExprKind::Cmp(cmp_op(op), bx(x), bx(y)), ty: Ty::Bool },
                    }
                }
                if lt == Ty::Bool && rt == Ty::Bool && matches!(op, BinOp::Eq | BinOp::Ne) {
                    return Expr { kind: ExprKind::Cmp(cmp_op(op), bx(x), bx(y)), ty: Ty::Bool };
                }
                // Structural: equal when every part is (floats as IEEE values).
                if compound(&lt) && lt == rt && matches!(op, BinOp::Eq | BinOp::Ne) {
                    return Expr { kind: ExprKind::Cmp(cmp_op(op), bx(x), bx(y)), ty: Ty::Bool };
                }
                if let (Ty::Param(_), true, true) = (&lt, lt == rt, op.is_comparison()) {
                    if self.satisfies(&lt, Constraint::Ordered) {
                        return Expr { kind: ExprKind::Cmp(cmp_op(op), bx(x), bx(y)), ty: Ty::Bool };
                    }
                    self.needs_constraint(&lt, Constraint::Ordered, e.span, &format!("`{}`", op.text()));
                    return Self::err_expr();
                }
                // i64 mixed with f64: point at the integer side.
                let mixed = match (&lt, &rt) {
                    (Ty::I64, Ty::F64(u)) => Some((l, &x, u.clone())),
                    (Ty::F64(u), Ty::I64) => Some((r, &y, u.clone())),
                    _ => None,
                };
                if let Some((side, sx, u)) = mixed {
                    if op != BinOp::Rem {
                        let msg = format!(
                            "`{}` cannot mix {} and {}; Anvil never converts implicitly",
                            op.text(),
                            self.show(&lt),
                            self.show(&rt)
                        );
                        let d = Diagnostic::new("E0300", e.span, msg);
                        let want = Ty::F64(u);
                        // Reuse the coercion fixes for the integer operand.
                        let before = self.diags.len();
                        self.require_ty(side, sx, &want);
                        let fixes = self.diags.pop().map(|d| d.fixes).unwrap_or_default();
                        debug_assert_eq!(self.diags.len(), before);
                        let mut d = d;
                        d.fixes = fixes;
                        self.err(d);
                        return Self::err_expr();
                    }
                }
            }
        }
        let (ls, rs) = (self.show(&lt), self.show(&rt));
        let mut d = Diagnostic::new("E0302", e.span, format!("`{}` is not defined for {ls} and {rs}", op.text()));
        if op == BinOp::Rem && matches!(lt, Ty::F64(_)) {
            d = d.note("% is only defined for i64", None);
        } else if lt == Ty::Str || rt == Ty::Str {
            d = d.note("build text with interpolation: \"{a} and {b}\"", None);
        } else if lt == rt && matches!(lt, Ty::Array(_) | Ty::Struct(_) | Ty::Enum(_) | Ty::Map(..)) {
            d = d.note("only i64, f64 and str are ordered; == and != compare any two values of one type", None);
        }
        self.err(d);
        Self::err_expr()
    }

    fn unit_fix(&self, d: Diagnostic, l: &ast::Expr, r: &ast::Expr, a: &Unit, c: &Unit) -> Diagnostic {
        // A bare literal on one side may just lack its unit, but that is a guess about intent.
        if let AK::Float(_, None) = &r.kind {
            if c.is_dimensionless() {
                return d.fix1(
                    FixKind::ChangesMeaning,
                    format!("give the literal the unit {a}"),
                    r.span.at_end(),
                    format!("[{a}]"),
                );
            }
        }
        if let AK::Float(_, None) = &l.kind {
            if a.is_dimensionless() {
                return d.fix1(
                    FixKind::ChangesMeaning,
                    format!("give the literal the unit {c}"),
                    l.span.at_end(),
                    format!("[{c}]"),
                );
            }
        }
        d
    }

    fn struct_lit(
        &mut self,
        b: &mut Body,
        e: &ast::Expr,
        name: &ast::Ident,
        fields: &[(ast::Ident, ast::Expr)],
        hint: Option<&Ty>,
    ) -> Expr {
        self.touch(&name.name);
        let generic = self.generic_types.get(&name.name).copied();
        let generic_sid = match generic {
            Some(g) if matches!(self.generics[g].kind, GenericKind::Struct(_)) => {
                self.resolve_shape(g);
                let shape = self.generics[g].fields.clone();
                let pairs: Vec<(&ast::Expr, Ty)> = fields
                    .iter()
                    .filter_map(|(f, v)| shape.iter().find(|(n, _)| *n == f.name).map(|(_, t)| (v, t.clone())))
                    .collect();
                let what = format!("`{}`", name.name);
                match self.infer_instance(b, g, hint, pairs, e.span, &what) {
                    Some(id) => Some(id),
                    None => {
                        for (_, v) in fields {
                            self.expr(b, v);
                        }
                        return Self::err_expr();
                    }
                }
            }
            _ => None,
        };
        let Some(sid) = generic_sid.or_else(|| self.struct_index.get(&name.name).copied()) else {
            if self.failed_imports.contains(&name.name) {
                for (_, v) in fields {
                    self.expr(b, v);
                }
                return Self::err_expr();
            }
            let mut d = Diagnostic::new("E0200", name.span, format!("unknown struct `{}`", name.name));
            if let Some(s) = closest(&name.name, self.struct_index.keys()) {
                d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), name.span, s);
            }
            self.err(d);
            for (_, v) in fields {
                self.expr(b, v);
            }
            return Self::err_expr();
        };
        let decl: Vec<(String, Ty)> = self.structs[sid].fields.clone();
        let def = self.type_spans.get(&name.name).copied();
        self.note_use(name.span, def);
        for (f, _) in fields {
            if let Some(i) = decl.iter().position(|(n, _)| *n == f.name) {
                let def = self.field_spans.get(&(sid, i)).copied();
                self.note_use(f.span, def);
            }
        }
        let mut ok = true;
        let mut by_name: HashMap<&str, usize> = HashMap::new();
        for (i, (f, _)) in fields.iter().enumerate() {
            if by_name.insert(f.name.as_str(), i).is_some() {
                self.err(Diagnostic::new("E0305", f.span, format!("field `{}` is given twice", f.name)));
                ok = false;
            }
            if !decl.iter().any(|(n, _)| *n == f.name) {
                self.field_index(sid, f);
                ok = false;
            }
        }
        let missing: Vec<&str> =
            decl.iter().filter(|(n, _)| !by_name.contains_key(n.as_str())).map(|(n, _)| n.as_str()).collect();
        if !missing.is_empty() {
            self.err(Diagnostic::new(
                "E0305",
                e.span,
                format!(
                    "struct `{}` literal is missing {}",
                    name.name,
                    missing.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(", ")
                ),
            ));
            ok = false;
        }
        let in_order = fields.iter().zip(decl.iter()).all(|((f, _), (n, _))| f.name == *n);
        if ok && !in_order {
            // Same set, wrong order: rewrite in declaration order.
            let text: Vec<String> = decl
                .iter()
                .map(|(n, _)| {
                    let (f, v) = &fields[by_name[n.as_str()]];
                    format!("{}: {}", f.name, self.text(v.span))
                })
                .collect();
            let inner = Span::new(fields[0].0.span.start, fields.last().unwrap().1.span.end);
            self.err(
                Diagnostic::new("E0305", e.span, format!("fields of `{}` must be in declaration order", name.name))
                    .note(
                        format!(
                            "declared order: {}",
                            decl.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
                        ),
                        None,
                    )
                    .fix1(FixKind::Mechanical, "reorder the fields", inner, text.join(", ")),
            );
            ok = false;
        }
        let mut vals = Vec::new();
        for (f, v) in fields {
            match decl.iter().find(|(n, _)| *n == f.name) {
                Some((_, t)) => vals.push(self.expect(b, v, &t.clone())),
                None => {
                    self.expr(b, v);
                }
            }
        }
        if !ok {
            return Self::err_expr();
        }
        Expr { kind: ExprKind::StructLit(sid, vals), ty: Ty::Struct(sid) }
    }

    fn note_effect(&mut self, b: &mut Body, e: Effect, span: Span, what: &str) {
        let doing = e.doing();
        if b.in_contract {
            self.err(Diagnostic::new("E0390", span, format!("{what} {doing}, which contracts cannot do")));
            return;
        }
        b.used.add(e);
        if b.uses.has(e) {
            return;
        }
        let who = if b.owner.is_some() { "this function" } else { "this test" };
        let whose = if b.uses.is_pure() {
            format!("{who} is pure")
        } else {
            format!("{who} does not declare uses {}", e.name())
        };
        // The effect goes where the formatter puts it: a clause lists io, fs, clock, random.
        let rank = |n: &str| EFFECTS.iter().position(|x| *x == n).unwrap_or(EFFECTS.len());
        let later = b.uses_list.iter().find(|u| rank(&u.name) > rank(e.name()));
        let (at, text) = match (b.uses_span, later) {
            (None, _) => (b.uses_insert, format!(" uses {}", e.name())),
            (Some(_), Some(l)) => (Span::new(l.span.start, l.span.start), format!("{}, ", e.name())),
            (Some(clause), None) => (clause.at_end(), format!(", {}", e.name())),
        };
        self.err(Diagnostic::new("E0330", span, format!("{what} {doing}, but {whose}")).fix1(
            FixKind::ChangesMeaning,
            format!("declare the effect: uses {}", e.name()),
            at,
            text,
        ));
    }

    /// Something that may allocate: an error in a `noalloc` function (E0333).
    fn note_alloc(&mut self, b: &mut Body, span: Span, what: &str) {
        let Some(kw) = b.noalloc else { return };
        let remove = Span::new(self.space_before(kw.start), kw.end);
        self.err(
            Diagnostic::new("E0333", span, format!("{what} may allocate, but this function is noalloc"))
                .note("promised here", Some(kw))
                .fix1(FixKind::ChangesMeaning, "remove the promise `noalloc`", remove, ""),
        );
    }

    /// A call from a `noalloc` function to one that makes no such promise (E0334).
    fn note_alloc_call(&mut self, b: &mut Body, span: Span, name: &str, callee: Option<FuncId>) {
        let Some(kw) = b.noalloc else { return };
        let mut d =
            Diagnostic::new("E0334", span, format!("calling `{name}`, which is not noalloc, from a noalloc function"))
                .note("promised here", Some(kw));
        if let Some(f) = callee.filter(|f| self.sigs[*f].module == self.module) {
            d = d.fix1(
                FixKind::ChangesMeaning,
                format!("promise that `{name}` does not allocate either"),
                self.sigs[f].noalloc_insert,
                " noalloc",
            );
        }
        self.err(d);
    }

    /// Where the spaces before `at` begin in the current file.
    fn space_before(&self, at: usize) -> usize {
        let text = self.src.text.as_bytes();
        let mut i = at;
        while i > 0 && text[i - 1] == b' ' {
            i -= 1;
        }
        i
    }

    fn call(&mut self, b: &mut Body, e: &ast::Expr, callee: &ast::Ident, args: &[ast::Arg]) -> Expr {
        let name = callee.name.as_str();
        if BUILTINS.contains(&name) {
            return self.builtin(b, e, callee, args);
        }
        if b.sees(name) {
            if let Some(l) = self.lookup_local(b, callee) {
                return self.call_value(b, e, callee, l, args);
            }
        }
        self.touch(name);
        if let (None, Some(&g)) = (self.func_index.get(name), self.generic_fns.get(name)) {
            return self.generic_call(b, e, callee, args, g, None);
        }
        let Some(&fid) = self.func_index.get(name) else {
            if self.failed_imports.contains(name) {
                for a in args {
                    self.expr(b, &a.value);
                }
                return Self::err_expr();
            }
            let mut cands: Vec<String> = self.func_index.keys().cloned().collect();
            cands.extend(BUILTINS.iter().map(|s| s.to_string()));
            let mut d = Diagnostic::new("E0200", callee.span, format!("unknown function `{name}`"));
            if let Some(module) = crate::std_module_with(name) {
                d = d.note(format!("std.{module} has it: import std.{module}.{{{name}}};"), None);
            } else if let Some(s) = closest(name, cands.iter()) {
                d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), callee.span, s);
            }
            self.err(d);
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        };
        let params: Vec<(String, Ty)> = self.sigs[fid].params.clone();
        let ret = self.sigs[fid].ret.clone();
        let local = self.sigs[fid].module == self.module;
        let decl = Loc { file: self.sigs[fid].module, span: self.sigs[fid].span };
        self.note_use(callee.span, Some(decl));
        for eff in self.sigs[fid].effects.iter() {
            self.note_effect(b, eff, callee.span, &format!("calling `{name}`"));
        }
        if !self.sigs[fid].noalloc {
            self.note_alloc_call(b, callee.span, name, Some(fid));
        }
        let sig_span = local.then_some(self.sigs[fid].span);
        let Some(ok) = self.check_args(name, e.span, args, &params, sig_span) else {
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        };
        let mut vals = Vec::new();
        for (a, (_, t)) in args.iter().zip(&params) {
            vals.push(self.expect(b, &a.value, t));
        }
        if !ok {
            return Self::err_expr();
        }
        Expr { kind: ExprKind::Call(fid, vals, self.site(e.span)), ty: ret }
    }

    /// Argument count and naming rules shared by functions and enum variants. None when the
    /// count is wrong (arguments cannot be matched to parameters); otherwise whether names are right.
    fn check_args(
        &mut self,
        name: &str,
        call_span: Span,
        args: &[ast::Arg],
        params: &[(String, Ty)],
        sig_span: Option<Span>,
    ) -> Option<bool> {
        if args.len() != params.len() {
            let list = params.iter().map(|(n, t)| format!("{n}: {}", self.show(t))).collect::<Vec<_>>().join(", ");
            self.err(
                Diagnostic::new(
                    "E0303",
                    call_span,
                    format!("`{name}` takes {} argument(s), found {}", params.len(), args.len()),
                )
                .note(format!("signature: {name}({list})"), sig_span),
            );
            return None;
        }
        let e_span = call_span;
        let mut ok = true;
        if params.len() == 1 {
            if let Some(n) = &args[0].name {
                ok = false;
                self.err(
                    Diagnostic::new(
                        "E0304",
                        n.span,
                        format!("`{name}` has one parameter, so the argument is not named"),
                    )
                    .fix1(
                        FixKind::Mechanical,
                        "remove the name",
                        Span::new(n.span.start, args[0].value.span.start),
                        "",
                    ),
                );
            }
        } else if params.len() >= 2 {
            let unnamed: Vec<usize> = (0..args.len()).filter(|i| args[*i].name.is_none()).collect();
            if unnamed.len() == args.len() {
                ok = false;
                let edits = args
                    .iter()
                    .zip(params)
                    .map(|(a, (p, _))| Edit { span: a.value.span.at_start(), text: format!("{p}: ") })
                    .collect();
                self.err(Diagnostic::new("E0304", e_span, format!("calls to `{name}` name every argument")).fix(
                    FixKind::Mechanical,
                    "name the arguments in parameter order",
                    edits,
                ));
            } else if !unnamed.is_empty() {
                ok = false;
                let a = &args[unnamed[0]];
                self.err(
                    Diagnostic::new("E0304", a.value.span, format!("argument {} is not named", unnamed[0] + 1)).fix1(
                        FixKind::Mechanical,
                        format!("name it `{}`", params[unnamed[0]].0),
                        a.value.span.at_start(),
                        format!("{}: ", params[unnamed[0]].0),
                    ),
                );
            } else {
                let names: Vec<&str> = args.iter().map(|a| a.name.as_ref().unwrap().name.as_str()).collect();
                let pnames: Vec<&str> = params.iter().map(|(p, _)| p.as_str()).collect();
                for a in args {
                    let n = a.name.as_ref().unwrap();
                    if !pnames.contains(&n.name.as_str()) {
                        ok = false;
                        let mut d = Diagnostic::new("E0304", n.span, format!("`{name}` has no parameter `{}`", n.name))
                            .note(format!("parameters: {}", pnames.join(", ")), None);
                        let owned: Vec<String> = pnames.iter().map(|s| s.to_string()).collect();
                        if let Some(s) = closest(&n.name, owned.iter()) {
                            d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), n.span, s);
                        }
                        self.err(d);
                    }
                }
                let mut sorted_a = names.clone();
                sorted_a.sort();
                let mut sorted_p = pnames.clone();
                sorted_p.sort();
                if ok && sorted_a != sorted_p {
                    ok = false;
                    self.err(Diagnostic::new("E0304", e_span, "each parameter is named exactly once"));
                } else if ok && names != pnames {
                    ok = false;
                    let text: Vec<String> = pnames
                        .iter()
                        .map(|p| {
                            let a = args.iter().find(|a| a.name.as_ref().unwrap().name == *p).unwrap();
                            format!("{p}: {}", self.text(a.value.span))
                        })
                        .collect();
                    let inner =
                        Span::new(args[0].name.as_ref().unwrap().span.start, args.last().unwrap().value.span.end);
                    self.err(
                        Diagnostic::new("E0304", e_span, "named arguments must be in parameter order")
                            .note(format!("parameter order: {}", pnames.join(", ")), None)
                            .fix1(FixKind::Mechanical, "reorder the arguments", inner, text.join(", ")),
                    );
                }
            }
        }
        Some(ok)
    }

    fn builtin(&mut self, b: &mut Body, e: &ast::Expr, callee: &ast::Ident, args: &[ast::Arg]) -> Expr {
        let name = callee.name.as_str();
        if name == "write_file" || name == "write_bytes" {
            return self.write_file(b, e, callee, args);
        }
        for a in args {
            if let Some(n) = &a.name {
                self.err(
                    Diagnostic::new("E0304", n.span, format!("built-in `{name}` takes positional arguments")).fix1(
                        FixKind::Mechanical,
                        "remove the name",
                        Span::new(n.span.start, a.value.span.start),
                        "",
                    ),
                );
            }
        }
        // Recorded first, so an error in an argument cannot also report the effect as unused.
        match name {
            "print" | "print_raw" | "eprint" | "exit" | "env" => {
                self.note_effect(b, Effect::Io, callee.span, &format!("`{name}`"))
            }
            "list_dir" => self.note_effect(b, Effect::Fs, callee.span, "`list_dir`"),
            "clock_ns" => self.note_effect(b, Effect::Clock, callee.span, "clock_ns"),
            "random_below" | "random_f64" | "random_seed" => {
                self.note_effect(b, Effect::Random, callee.span, &format!("`{name}`"))
            }
            "args" | "read_line" | "read_stdin" => self.note_effect(b, Effect::Io, callee.span, &format!("`{name}`")),
            "read_file" | "read_bytes" => self.note_effect(b, Effect::Fs, callee.span, &format!("`{name}`")),
            _ => {}
        }
        if matches!(
            name,
            "args" | "read_line" | "read_stdin" | "read_file" | "read_bytes" | "bytes_of" | "env" | "list_dir"
        ) {
            self.note_alloc(b, callee.span, &format!("`{name}`"));
        }
        let vals: Vec<Expr> = args.iter().map(|a| self.expr(b, &a.value)).collect();
        if vals.iter().any(|v| v.ty == Ty::Error) {
            return Self::err_expr();
        }
        if name == "print" || name == "eprint" {
            if vals.len() != 1 {
                let mut d =
                    Diagnostic::new("E0303", e.span, format!("{name} takes one argument, found {}", vals.len()))
                        .note("build the line with interpolation: print(\"x = {x}\")", None);
                if let Some(text) = self.as_interpolation(args) {
                    let inner = Span::new(args[0].value.span.start, args.last().unwrap().value.span.end);
                    d = d.fix1(FixKind::Mechanical, "join the arguments into one interpolated string", inner, text);
                }
                self.err(d);
                return Self::err_expr();
            }
            if matches!(vals[0].ty, Ty::Param(_)) && !self.satisfies(&vals[0].ty, Constraint::Text) {
                self.needs_constraint(&vals[0].ty, Constraint::Text, args[0].value.span, "printing it");
                return Self::err_expr();
            }
            if !printable(&vals[0].ty) && !matches!(vals[0].ty, Ty::Param(_)) {
                let t = self.show(&vals[0].ty);
                let hint = match vals[0].ty {
                    Ty::Struct(_) => "print the fields one by one",
                    Ty::Array(_) => "print the elements in a loop",
                    Ty::Enum(_) => "match on it and print each case",
                    Ty::Map(..) => "print the entries in a loop over m.keys()",
                    Ty::Bytes => "print b.hex(), or b.to_str() when they are text",
                    _ => "only values have text",
                };
                self.err(
                    Diagnostic::new(
                        "E0407",
                        args[0].value.span,
                        format!("{name} takes i64, f64, bool or str, not {t}"),
                    )
                    .note(hint, None),
                );
                return Self::err_expr();
            }
            let b = if name == "print" { Builtin::Print } else { Builtin::EPrint };
            return Expr { kind: ExprKind::Builtin(b, vals), ty: Ty::Void };
        }
        if name == "clock_ns" || name == "random_f64" || name == "args" || name == "read_line" || name == "read_stdin" {
            if !vals.is_empty() {
                self.err(Diagnostic::new(
                    "E0303",
                    e.span,
                    format!("`{name}` takes no arguments, found {}", vals.len()),
                ));
                return Self::err_expr();
            }
            let site = self.site(e.span);
            return match name {
                "clock_ns" => Expr { kind: ExprKind::Builtin(Builtin::ClockNs, vals), ty: Ty::I64 },
                "args" => Expr { kind: ExprKind::Builtin(Builtin::Args, vals), ty: Ty::Array(Box::new(Ty::Str)) },
                "read_stdin" => Expr { kind: ExprKind::Builtin(Builtin::ReadStdin, vals), ty: Ty::Bytes },
                "read_line" => Expr {
                    kind: ExprKind::Builtin(Builtin::ReadLine { message: read_line_message(&site) }, vals),
                    ty: self.option_of(Ty::Str),
                },
                _ => Expr { kind: ExprKind::Builtin(Builtin::RandomF64, vals), ty: Ty::F64(Unit::dimensionless()) },
            };
        }
        if vals.len() != 1 {
            self.err(Diagnostic::new("E0303", e.span, format!("`{name}` takes 1 argument, found {}", vals.len())));
            return Self::err_expr();
        }
        let arg = &args[0].value;
        let site = self.site(e.span);
        let x = vals.into_iter().next().unwrap();
        let t = x.ty.clone();
        let bad = |c: &mut Self, want: &str| {
            let got = c.show(&t);
            c.err(Diagnostic::new("E0300", arg.span, format!("`{name}` takes {want}, found {got}")));
            Self::err_expr()
        };
        match name {
            "assert" => {
                if t != Ty::Bool {
                    return bad(self, "bool");
                }
                let message = format!("assertion failed at {site}: {}", self.text(arg.span));
                // A comparison shows both sides when it fails.
                if let ExprKind::Cmp(op, l, r) = &x.kind {
                    if !matches!(l.ty, Ty::Param(_) | Ty::Error) {
                        let sides = vec![(**l).clone(), (**r).clone()];
                        return Expr { kind: ExprKind::Builtin(Builtin::AssertCmp { message, op: *op }, sides), ty: Ty::Void };
                    }
                }
                Expr { kind: ExprKind::Builtin(Builtin::Assert { message }, vec![x]), ty: Ty::Void }
            }
            "sqrt" => match &t {
                Ty::F64(u) => match u.sqrt() {
                    Some(r) => Expr { kind: ExprKind::Builtin(Builtin::Sqrt, vec![x]), ty: Ty::F64(r) },
                    None => {
                        let t = self.show(&t);
                        self.err(Diagnostic::new("E0360", arg.span, format!("sqrt of {t} has no unit")));
                        Self::err_expr()
                    }
                },
                _ => bad(self, "f64"),
            },
            "abs" => match &t {
                Ty::I64 => Expr {
                    kind: ExprKind::Builtin(Builtin::AbsI64 { message: overflow_message("abs", &site) }, vec![x]),
                    ty: Ty::I64,
                },
                Ty::F64(_) => Expr { kind: ExprKind::Builtin(Builtin::AbsF64, vec![x]), ty: t.clone() },
                _ => bad(self, "i64 or f64"),
            },
            "to_f64" => {
                if t != Ty::I64 {
                    return bad(self, "i64");
                }
                Expr { kind: ExprKind::Builtin(Builtin::ToF64, vec![x]), ty: Ty::F64(Unit::dimensionless()) }
            }
            "to_i64" => match &t {
                Ty::F64(u) if u.is_dimensionless() => Expr {
                    kind: ExprKind::Builtin(
                        Builtin::ToI64 { message: format!("to_i64 of a non-finite or out-of-range value at {site}") },
                        vec![x],
                    ),
                    ty: Ty::I64,
                },
                Ty::F64(u) => {
                    let t = self.show(&t);
                    self.err(
                        Diagnostic::new("E0301", arg.span, format!("to_i64 takes a dimensionless f64, found {t}"))
                            .fix1(
                                FixKind::ChangesMeaning,
                                format!("divide by 1.0[{u}] to state the unit you are counting in"),
                                arg.span.at_end(),
                                format!(" / 1.0[{u}]"),
                            ),
                    );
                    Self::err_expr()
                }
                _ => bad(self, "f64"),
            },
            "random_below" => {
                if t != Ty::I64 {
                    return bad(self, "i64");
                }
                Expr { kind: ExprKind::Builtin(Builtin::RandomBelow { site }, vec![x]), ty: Ty::I64 }
            }
            "random_seed" => {
                if t != Ty::I64 {
                    return bad(self, "i64");
                }
                Expr { kind: ExprKind::Builtin(Builtin::RandomSeed, vec![x]), ty: Ty::Void }
            }
            "read_file" | "read_bytes" => {
                if t != Ty::Str {
                    return bad(self, "a str path");
                }
                let (content, b) =
                    if name == "read_file" { (Ty::Str, Builtin::ReadFile) } else { (Ty::Bytes, Builtin::ReadBytes) };
                let ty = self.result_of(content, Ty::Enum(self.file_error));
                Expr { kind: ExprKind::Builtin(b, vec![x]), ty }
            }
            "exit" => {
                if t != Ty::I64 {
                    return bad(self, "an i64 exit code");
                }
                Expr { kind: ExprKind::Builtin(Builtin::Exit { site }, vec![x]), ty: Ty::Void }
            }
            "panic" => {
                if t != Ty::Str {
                    return bad(self, "a str message");
                }
                Expr { kind: ExprKind::Builtin(Builtin::Panic { site }, vec![x]), ty: Ty::Void }
            }
            "print_raw" => {
                if t != Ty::Str {
                    return bad(self, "a str");
                }
                Expr { kind: ExprKind::Builtin(Builtin::PrintRaw, vec![x]), ty: Ty::Void }
            }
            "env" => {
                if t != Ty::Str {
                    return bad(self, "a str name");
                }
                let ty = self.option_of(Ty::Str);
                Expr { kind: ExprKind::Builtin(Builtin::Env, vec![x]), ty }
            }
            "list_dir" => {
                if t != Ty::Str {
                    return bad(self, "a str path");
                }
                let ty = self.result_of(Ty::Array(Box::new(Ty::Str)), Ty::Enum(self.file_error));
                Expr { kind: ExprKind::Builtin(Builtin::ListDir, vec![x]), ty }
            }
            "bytes_of" => {
                if t != Ty::Array(Box::new(Ty::I64)) {
                    return bad(self, "[i64]");
                }
                Expr { kind: ExprKind::Builtin(Builtin::BytesOf { site }, vec![x]), ty: Ty::Bytes }
            }
            _ => unreachable!("builtin {name}"),
        }
    }

    /// `write_file(path: p, text: t)` and `write_bytes(path: p, data: d)`: two parameters, so
    /// named like any call with two.
    fn write_file(&mut self, b: &mut Body, e: &ast::Expr, callee: &ast::Ident, args: &[ast::Arg]) -> Expr {
        let name = callee.name.as_str();
        self.note_effect(b, Effect::Fs, callee.span, &format!("`{name}`"));
        let (content, builtin) = if name == "write_file" {
            (("text", Ty::Str), Builtin::WriteFile)
        } else {
            (("data", Ty::Bytes), Builtin::WriteBytes)
        };
        let params = vec![("path".to_string(), Ty::Str), (content.0.to_string(), content.1)];
        let Some(ok) = self.check_args(name, e.span, args, &params, None) else {
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        };
        let vals: Vec<Expr> = args.iter().zip(&params).map(|(a, (_, t))| self.expect(b, &a.value, t)).collect();
        if !ok || vals.iter().any(|v| v.ty == Ty::Error) {
            return Self::err_expr();
        }
        let ty = self.result_of(Ty::I64, Ty::Enum(self.file_error));
        Expr { kind: ExprKind::Builtin(builtin, vals), ty }
    }
}

impl<'a> Checker<'a> {
    /// `print(a, " ", b)` as `"{a} {b}"`, when every argument can be written inside a string.
    fn as_interpolation(&self, args: &[ast::Arg]) -> Option<String> {
        let mut out = String::from("\"");
        for a in args {
            let text = self.text(a.value.span);
            match &a.value.kind {
                AK::Str(_) | AK::Interp(_) => {
                    let inner = &text[1..text.len() - 1];
                    if matches!(a.value.kind, AK::Str(_)) {
                        out.push_str(&inner.replace('{', "{{").replace('}', "}}"));
                    } else {
                        out.push_str(inner);
                    }
                }
                _ => {
                    if text.contains(['"', '{', '}', '\n']) || a.name.is_some() {
                        return None;
                    }
                    out.push('{');
                    out.push_str(&text);
                    out.push('}');
                }
            }
        }
        out.push('"');
        Some(out)
    }

    /// The generic enum named by `base` in `Tree.Leaf(x)`, unless a local has that name.
    fn generic_enum(&self, b: &Body, base: &ast::Expr) -> Option<usize> {
        let AK::Name(n) = &base.kind else { return None };
        if b.sees(&n.name) {
            return None;
        }
        self.touch(&n.name);
        self.generic_types.get(&n.name).copied().filter(|g| matches!(self.generics[*g].kind, GenericKind::Enum(_)))
    }

    /// The enum named by `base` in `Shape.Circle`, unless a local has that name.
    fn enum_name(&self, b: &Body, base: &ast::Expr) -> Option<EnumId> {
        let AK::Name(n) = &base.kind else { return None };
        if b.sees(&n.name) {
            return None;
        }
        self.touch(&n.name);
        if n.name == "FileError" {
            return Some(self.file_error);
        }
        self.enum_index.get(&n.name).copied()
    }

    /// `Shape.Circle(r)` or `Shape.Empty` (args None).
    fn variant_ctor(
        &mut self,
        b: &mut Body,
        e: &ast::Expr,
        (enum_id, enum_span): (EnumId, Span),
        vname: &ast::Ident,
        args: Option<&[ast::Arg]>,
    ) -> Expr {
        let ename = self.enums[enum_id].name.clone();
        let def = self.type_spans.get(&ename).copied();
        self.note_use(enum_span, def);
        let variants = self.enums[enum_id].variants.clone();
        let Some(vi) = variants.iter().position(|v| v.name == vname.name) else {
            let names: Vec<String> = variants.iter().map(|v| v.name.clone()).collect();
            let mut d = Diagnostic::new("E0200", vname.span, format!("`{ename}` has no variant `{}`", vname.name))
                .note(format!("variants: {}", names.join(", ")), None);
            if let Some(s) = closest(&vname.name, names.iter()) {
                d = d.fix1(FixKind::ChangesMeaning, format!("did you mean `{s}`?"), vname.span, s);
            }
            self.err(d);
            for a in args.unwrap_or(&[]) {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        };
        let fields = variants[vi].fields.clone();
        let full = format!("{ename}.{}", vname.name);
        let def = self.variant_spans.get(&(enum_id, vi)).copied();
        self.note_use(vname.span, def);
        let Some(args) = args else {
            if fields.is_empty() {
                return Expr {
                    kind: ExprKind::Variant { enum_id, variant: vi, fields: Vec::new() },
                    ty: Ty::Enum(enum_id),
                };
            }
            let list = fields.iter().map(|(n, t)| format!("{n}: {}", self.show(t))).collect::<Vec<_>>().join(", ");
            self.err(Diagnostic::new("E0303", e.span, format!("`{full}` has fields; construct it as {full}({list})")));
            return Self::err_expr();
        };
        if fields.is_empty() {
            let mut d = Diagnostic::new("E0303", e.span, format!("`{full}` has no fields"));
            if args.is_empty() {
                d = d.fix1(FixKind::Mechanical, "remove the parentheses", Span::new(vname.span.end, e.span.end), "");
            }
            self.err(d);
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        }
        let Some(ok) = self.check_args(&full, e.span, args, &fields, None) else {
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        };
        let vals: Vec<Expr> = args.iter().zip(&fields).map(|(a, (_, t))| self.expect(b, &a.value, t)).collect();
        if !ok {
            return Self::err_expr();
        }
        Expr { kind: ExprKind::Variant { enum_id, variant: vi, fields: vals }, ty: Ty::Enum(enum_id) }
    }

    /// `Some(x)`, `None`, `Ok(x)`, `Err(e)`. Only `Some` can be typed without an expected type.
    fn builtin_ctor(
        &mut self,
        b: &mut Body,
        e: &ast::Expr,
        callee: &ast::Ident,
        args: Option<&[ast::Arg]>,
        hint: Option<&Ty>,
    ) -> Expr {
        let name = callee.name.as_str();
        let kind = if name == "Some" || name == "None" { "Option" } else { "Result" };
        let hinted = match hint {
            Some(Ty::Enum(id)) if self.builtin_kind(*id) == Some(kind) => Some(*id),
            _ => None,
        };
        let variant = match name {
            "Some" => OPTION_SOME,
            "None" => OPTION_NONE,
            "Ok" => RESULT_OK,
            _ => RESULT_ERR,
        };
        // The value, if this constructor takes one.
        let arg = if name == "None" {
            if let Some(args) = args {
                let mut d = Diagnostic::new("E0303", e.span, "None takes no value");
                if args.is_empty() {
                    d = d.fix1(
                        FixKind::Mechanical,
                        "remove the parentheses",
                        Span::new(callee.span.end, e.span.end),
                        "",
                    );
                }
                self.err(d);
                return Self::err_expr();
            }
            None
        } else {
            let args = args.unwrap_or(&[]);
            if args.len() != 1 {
                self.err(Diagnostic::new("E0303", e.span, format!("`{name}` takes one value: {name}(x)")));
                for a in args {
                    self.expr(b, &a.value);
                }
                return Self::err_expr();
            }
            if let Some(n) = &args[0].name {
                self.err(Diagnostic::new("E0304", n.span, format!("`{name}` takes its value without a name")).fix1(
                    FixKind::Mechanical,
                    "remove the name",
                    Span::new(n.span.start, args[0].value.span.start),
                    "",
                ));
                return Self::err_expr();
            }
            Some(&args[0].value)
        };
        let enum_id = match hinted {
            Some(id) => id,
            None if name == "Some" => {
                let x = self.expr(b, arg.unwrap());
                if x.ty == Ty::Error {
                    return Self::err_expr();
                }
                let Ty::Enum(id) = self.option_of(x.ty.clone()) else { unreachable!() };
                return Expr { kind: ExprKind::Variant { enum_id: id, variant, fields: vec![x] }, ty: Ty::Enum(id) };
            }
            None => {
                if let Some(a) = arg {
                    self.expr(b, a);
                }
                match hint {
                    Some(t) if *t != Ty::Error => {
                        let t = self.show(t);
                        self.err(Diagnostic::new("E0300", e.span, format!("expected {t}, found {kind}")));
                    }
                    Some(_) => {}
                    None => {
                        let example = if kind == "Option" {
                            "let x: Option[i64] = None;"
                        } else {
                            "let r: Result[i64, str] = Err(\"bad input\");"
                        };
                        self.err(
                            Diagnostic::new("E0404", e.span, format!("the type of `{name}` cannot be inferred here"))
                                .note(format!("state the type: {example}"), None),
                        );
                    }
                }
                return Self::err_expr();
            }
        };
        let fields = match arg {
            None => Vec::new(),
            Some(a) => {
                let t = self.enums[enum_id].variants[variant].fields[0].1.clone();
                vec![self.expect(b, a, &t)]
            }
        };
        Expr { kind: ExprKind::Variant { enum_id, variant, fields }, ty: Ty::Enum(enum_id) }
    }

    fn method(
        &mut self,
        b: &mut Body,
        e: &ast::Expr,
        receiver: &ast::Expr,
        method: &ast::Ident,
        args: &[ast::Arg],
        hint: Option<&Ty>,
    ) -> Expr {
        if let Some(g) = self.generic_enum(b, receiver) {
            self.resolve_shape(g);
            let fields: Vec<(String, Ty)> = self.generics[g]
                .variants
                .iter()
                .find(|v| v.name == method.name)
                .map(|v| v.fields.clone())
                .unwrap_or_default();
            let pairs: Vec<(&ast::Expr, Ty)> = args
                .iter()
                .enumerate()
                .filter_map(|(i, a)| {
                    let by_name = a.name.as_ref().and_then(|n| fields.iter().find(|(f, _)| *f == n.name));
                    by_name.or(fields.get(i)).map(|(_, t)| (&a.value, t.clone()))
                })
                .collect();
            let what = format!("`{}.{}`", self.generics[g].name, method.name);
            return match self.infer_instance(b, g, hint, pairs, e.span, &what) {
                Some(id) => self.variant_ctor(b, e, (id, receiver.span), method, Some(args)),
                None => {
                    for a in args {
                        self.expr(b, &a.value);
                    }
                    Self::err_expr()
                }
            };
        }
        if let Some(eid) = self.enum_name(b, receiver) {
            return self.variant_ctor(b, e, (eid, receiver.span), method, Some(args));
        }
        let m = method.name.as_str();
        // The receiver's type says whose method this is. Built-in mutating methods need the
        // receiver as a place, so its type is learned quietly first.
        let mutating = matches!(m, "push" | "pop" | "set" | "remove");
        let pre = if mutating { None } else { Some(self.expr(b, receiver)) };
        let rt = match &pre {
            Some(x) => x.ty.clone(),
            None => {
                self.quiet += 1;
                let t = self.expr(b, receiver).ty;
                self.quiet -= 1;
                t
            }
        };
        if self.has_methods(&rt) {
            return self.user_method(b, e, receiver, (rt, pre), method, args);
        }
        // `set` and `replace` name their two arguments, like any call with two parameters; the
        // rest take one or none.
        if m != "set" && m != "replace" && m != "find_from" {
            for a in args {
                if let Some(n) = &a.name {
                    self.err(Diagnostic::new("E0304", n.span, format!("`{m}` takes positional arguments")).fix1(
                        FixKind::Mechanical,
                        "remove the name",
                        Span::new(n.span.start, a.value.span.start),
                        "",
                    ));
                }
            }
        }
        let want_args = match m {
            "pop" | "len" | "keys" | "values" | "trim" | "parse_i64" | "parse_f64" | "to_bytes" | "to_str" | "hex"
            | "to_array" | "to_upper" | "to_lower" | "chars" | "sorted" | "reversed" | "bit_len" | "leading_zeros"
            | "trailing_zeros" => Some(0),
            "push" | "remove" | "has" | "get" | "contains" | "find" | "starts_with" | "ends_with" | "split" | "xor"
            | "unsigned_shr" | "wrapping_add" | "wrapping_sub" | "wrapping_mul" | "join" | "fixed"
            | "parallel_map" | "word_at" => Some(1),
            "replace" | "find_from" => Some(2),
            _ => None,
        };
        if let Some(want) = want_args {
            if args.len() != want {
                self.err(Diagnostic::new(
                    "E0303",
                    e.span,
                    format!("`{m}` takes {want} argument(s), found {}", args.len()),
                ));
                for a in args {
                    self.expr(b, &a.value);
                }
                return Self::err_expr();
            }
        }
        if mutating {
            return self.mutating_method(b, e, receiver, method, args);
        }
        let x = pre.expect("a method that does not mutate has its receiver checked");
        if x.ty == Ty::Error {
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        }
        let site = self.site(e.span);
        if let ("replace", Ty::Str) = (m, &x.ty) {
            let params = [("old".to_string(), Ty::Str), ("new".to_string(), Ty::Str)];
            let ok = self.check_args("replace", e.span, args, &params, None) == Some(true);
            let mut vals = vec![x];
            vals.extend(args.iter().map(|a| self.expect(b, &a.value, &Ty::Str)));
            if !ok {
                return Self::err_expr();
            }
            self.note_alloc(b, e.span, "`replace`");
            let builtin = Builtin::StrReplace { message: replace_message(&site) };
            return Expr { kind: ExprKind::Builtin(builtin, vals), ty: Ty::Str };
        }
        if let ("find_from", Ty::Str) = (m, &x.ty) {
            let params = [("t".to_string(), Ty::Str), ("at".to_string(), Ty::I64)];
            let ok = self.check_args("find_from", e.span, args, &params, None) == Some(true);
            let mut vals = vec![x];
            for (a, (_, want)) in args.iter().zip(&params) {
                vals.push(self.expect(b, &a.value, want));
            }
            if !ok {
                return Self::err_expr();
            }
            let ty = self.option_of(Ty::I64);
            return Expr { kind: ExprKind::Builtin(Builtin::StrFindFrom { site }, vals), ty };
        }
        if let ("parallel_map", Ty::Array(elem)) = (m, &x.ty) {
            let elem = (**elem).clone();
            let f = self.expr(b, &args[0].value);
            let want = format!("fn({}) -> U", self.show(&elem));
            let ret = match &f.ty {
                Ty::Error => return Self::err_expr(),
                Ty::Fn(ft) if ft.params == [elem.clone()] && ft.ret != Ty::Void => {
                    if !ft.effects.is_pure() {
                        let used: Vec<&str> = ft.effects.iter().map(Effect::name).collect();
                        self.err(
                            Diagnostic::new(
                                "E0416",
                                args[0].value.span,
                                format!("parallel work must be pure, and this function uses {}", used.join(", ")),
                            )
                            .note("the workers run at once, in no fixed order; do the effects before or after parallel_map", None),
                        );
                        return Self::err_expr();
                    }
                    ft.ret.clone()
                }
                other => {
                    let t = self.show(other);
                    self.err(Diagnostic::new(
                        "E0300",
                        args[0].value.span,
                        format!("parallel_map takes {want} for each element, found {t}"),
                    ));
                    return Self::err_expr();
                }
            };
            self.note_alloc(b, e.span, "`parallel_map`");
            return Expr { kind: ExprKind::Builtin(Builtin::ParallelMap, vec![x, f]), ty: Ty::Array(Box::new(ret)) };
        }
        let (builtin, ty) = match (m, x.ty.clone()) {
            ("len", Ty::Str | Ty::Bytes | Ty::Array(_) | Ty::Map(..)) => (Builtin::Len, Ty::I64),
            ("to_upper", Ty::Str) => (Builtin::StrUpper, Ty::Str),
            ("to_lower", Ty::Str) => (Builtin::StrLower, Ty::Str),
            ("chars", Ty::Str) => (Builtin::StrChars, Ty::Array(Box::new(Ty::Str))),
            ("join", Ty::Array(e)) if *e == Ty::Str => (Builtin::ArrJoin, Ty::Str),
            ("fixed", Ty::F64(_)) => (Builtin::FloatFixed { message: fixed_message(&site) }, Ty::Str),
            ("reversed", t @ Ty::Array(_)) => (Builtin::ArrReversed, t),
            ("sorted", Ty::Array(e)) if matches!(*e, Ty::I64 | Ty::F64(_) | Ty::Str) => {
                (Builtin::ArrSorted { message: sorted_message(&site) }, Ty::Array(e))
            }
            ("xor" | "unsigned_shr", Ty::I64) => {
                let other = self.expect(b, &args[0].value, &Ty::I64);
                let op = if m == "xor" { BitOp::Xor } else { BitOp::UShr };
                let kind = ExprKind::Bits { op, l: Box::new(x), r: Box::new(other), site };
                return Expr { kind, ty: Ty::I64 };
            }
            ("wrapping_add", Ty::I64) => (Builtin::WrappingAdd, Ty::I64),
            ("wrapping_sub", Ty::I64) => (Builtin::WrappingSub, Ty::I64),
            ("wrapping_mul", Ty::I64) => (Builtin::WrappingMul, Ty::I64),
            ("bit_len", Ty::I64) => (Builtin::BitLen, Ty::I64),
            ("leading_zeros", Ty::I64) => (Builtin::LeadingZeros, Ty::I64),
            ("trailing_zeros", Ty::I64) => (Builtin::TrailingZeros, Ty::I64),
            ("to_bytes", Ty::Str) => (Builtin::StrToBytes, Ty::Bytes),
            ("to_str", Ty::Bytes) => (Builtin::BytesToStr, self.option_of(Ty::Str)),
            ("hex", Ty::Bytes) => (Builtin::BytesHex, Ty::Str),
            ("to_array", Ty::Bytes) => (Builtin::BytesToArray, Ty::Array(Box::new(Ty::I64))),
            ("word_at", Ty::Bytes) => (Builtin::BytesWordAt { site }, Ty::I64),
            ("contains", Ty::Str) => (Builtin::StrContains, Ty::Bool),
            ("starts_with", Ty::Str) => (Builtin::StrStartsWith, Ty::Bool),
            ("ends_with", Ty::Str) => (Builtin::StrEndsWith, Ty::Bool),
            ("find", Ty::Str) => (Builtin::StrFind, self.option_of(Ty::I64)),
            ("split", Ty::Str) => (Builtin::StrSplit { message: split_message(&site) }, Ty::Array(Box::new(Ty::Str))),
            ("trim", Ty::Str) => (Builtin::StrTrim, Ty::Str),
            ("parse_i64", Ty::Str) => (Builtin::StrParseI64, self.option_of(Ty::I64)),
            ("parse_f64", Ty::Str) => (Builtin::StrParseF64, self.option_of(Ty::F64(Unit::dimensionless()))),
            ("has", Ty::Map(..)) => (Builtin::MapHas, Ty::Bool),
            ("get", Ty::Map(_, v)) => (Builtin::MapGet, self.option_of(*v)),
            ("keys", Ty::Map(k, _)) => (Builtin::MapKeys, Ty::Array(k)),
            ("values", Ty::Map(_, v)) => (Builtin::MapValues, Ty::Array(v)),
            _ => {
                for a in args {
                    self.expr(b, &a.value);
                }
                let t = self.show(&x.ty);
                let have = match x.ty {
                    Ty::Array(_) => {
                        "arrays have len, push, pop, reversed and parallel_map; sorted on [i64], [f64] and [str]; \
                         join on [str]"
                    }
                    Ty::F64(_) => "f64 has fixed",
                    Ty::Str => {
                        "str has len, contains, find, starts_with, ends_with, split, trim, parse_i64, parse_f64, \
                         to_bytes, replace, to_upper, to_lower and chars"
                    }
                    Ty::I64 => {
                        "i64 has xor, unsigned_shr, wrapping_add, wrapping_sub, wrapping_mul, bit_len, leading_zeros \
                         and trailing_zeros"
                    }
                    Ty::Bytes => "bytes have len, to_str, hex, to_array and word_at",
                    Ty::Map(..) => "maps have len, has, get, set, remove, keys and values",
                    _ => "only arrays, str and maps have methods; write a function that takes the value",
                };
                let mut d = Diagnostic::new("E0406", method.span, format!("{t} has no method `{m}`")).note(have, None);
                if let Some(module) = crate::std_module_with(m) {
                    d = d.note(format!("std.{module} has it as a function: import std.{module}.{{{m}}};"), None);
                }
                self.err(d);
                return Self::err_expr();
            }
        };
        if matches!(
            builtin,
            Builtin::StrSplit { .. }
                | Builtin::StrTrim
                | Builtin::MapKeys
                | Builtin::MapValues
                | Builtin::StrToBytes
                | Builtin::BytesToStr
                | Builtin::BytesHex
                | Builtin::BytesToArray
                | Builtin::StrUpper
                | Builtin::StrLower
                | Builtin::StrChars
                | Builtin::ArrJoin
                | Builtin::FloatFixed { .. }
                | Builtin::ArrSorted { .. }
                | Builtin::ArrReversed
        ) {
            self.note_alloc(b, e.span, &format!("`{m}`"));
        }
        let arg_ty = match &x.ty {
            Ty::Map(k, _) => (**k).clone(),
            Ty::I64 | Ty::F64(_) | Ty::Bytes => Ty::I64,
            _ => Ty::Str,
        };
        let mut vals = vec![x];
        for a in args {
            vals.push(self.expect(b, &a.value, &arg_ty));
        }
        Expr { kind: ExprKind::Builtin(builtin, vals), ty }
    }

    /// `push` and `pop` on arrays, `set` and `remove` on maps: they change a value in place, so
    /// the receiver must be a var (or a field or element of one).
    fn mutating_method(
        &mut self,
        b: &mut Body,
        e: &ast::Expr,
        receiver: &ast::Expr,
        method: &ast::Ident,
        args: &[ast::Arg],
    ) -> Expr {
        let m = method.name.as_str();
        let kind = if matches!(m, "push" | "pop") { "an array" } else { "a map" };
        if !is_place(receiver) {
            let x = self.expr(b, receiver);
            for a in args {
                self.expr(b, &a.value);
            }
            if x.ty != Ty::Error {
                let example = if kind == "an array" {
                    "bind the array first: var xs: [i64] = make(); xs.push(1);"
                } else {
                    "bind the map first: var m: Map[str, i64] = make(); m.set(key: \"a\", value: 1);"
                };
                self.err(
                    Diagnostic::new(
                        "E0406",
                        receiver.span,
                        format!("`{m}` changes {kind} in place, so it needs a var"),
                    )
                    .note(example, None),
                );
            }
            return Self::err_expr();
        }
        let Some((local, path, ty)) = self.place(b, receiver) else {
            for a in args {
                self.expr(b, &a.value);
            }
            return Self::err_expr();
        };
        match (m, ty) {
            ("push", Ty::Array(elem)) => {
                self.note_alloc(b, e.span, "`push`");
                let v = self.expect(b, &args[0].value, &elem);
                Expr { kind: ExprKind::Push { local, path, value: Box::new(v), at: self.site(e.span) }, ty: Ty::Void }
            }
            ("pop", Ty::Array(elem)) => {
                // pop reads the array; push and assignment only write it.
                b.meta[local].used = true;
                self.note_alloc(b, e.span, "`pop`");
                let message = pop_message(&self.site(e.span));
                Expr { kind: ExprKind::Pop { local, path, message, at: self.site(e.span) }, ty: *elem }
            }
            ("set", Ty::Map(k, v)) => {
                self.note_alloc(b, e.span, "`set`");
                let params = vec![("key".to_string(), *k), ("value".to_string(), *v)];
                if self.check_args("set", e.span, args, &params, None) != Some(true) {
                    for a in args {
                        self.expr(b, &a.value);
                    }
                    return Self::err_expr();
                }
                let key = self.expect(b, &args[0].value, &params[0].1);
                let value = self.expect(b, &args[1].value, &params[1].1);
                Expr {
                    kind: ExprKind::MapSet { local, path, key: Box::new(key), value: Box::new(value), at: self.site(e.span) },
                    ty: Ty::Void,
                }
            }
            ("remove", Ty::Map(k, _)) => {
                // remove reads the map: it fails when the key is not there.
                b.meta[local].used = true;
                self.note_alloc(b, e.span, "`remove`");
                let key = self.expect(b, &args[0].value, &k);
                let missing = missing_key_message(&self.site(e.span));
                let at = self.site(e.span);
                Expr { kind: ExprKind::MapRemove { local, path, key: Box::new(key), missing, at }, ty: Ty::Void }
            }
            (_, Ty::Error) => {
                for a in args {
                    self.expr(b, &a.value);
                }
                Self::err_expr()
            }
            (_, other) => {
                for a in args {
                    self.expr(b, &a.value);
                }
                let t = self.show(&other);
                self.err(Diagnostic::new("E0406", method.span, format!("`{m}` is {kind} method; {t} has no `{m}`")));
                Self::err_expr()
            }
        }
    }
}

/// Syntactically a variable, a field of one or an element of one.
fn is_place(e: &ast::Expr) -> bool {
    match &e.kind {
        AK::Name(_) => true,
        AK::Field(b, _) | AK::Index(b, _) | AK::Paren(b) => is_place(b),
        _ => false,
    }
}

/// `a op b` for two literals of a constant, as running it would give.
fn fold_binary(op: BinOp, a: &Expr, b: &Expr) -> Result<Expr, String> {
    fn compare<T: PartialOrd + ?Sized>(op: BinOp, a: &T, b: &T) -> bool {
        match op {
            BinOp::Eq => a == b,
            BinOp::Ne => a != b,
            BinOp::Lt => a < b,
            BinOp::Le => a <= b,
            BinOp::Gt => a > b,
            _ => a >= b,
        }
    }
    let boolean = |v| Ok(Expr { kind: ExprKind::Bool(v), ty: Ty::Bool });
    let mismatch = || format!("`{}` cannot join these values in a constant", op.text());
    match (&a.kind, &b.kind) {
        (ExprKind::Int(x), ExprKind::Int(y)) => {
            let (x, y) = (*x, *y);
            if op.is_comparison() {
                return boolean(compare(op, &x, &y));
            }
            let shift = (0..=63).contains(&y);
            let out = match op {
                BinOp::Add => x.checked_add(y),
                BinOp::Sub => x.checked_sub(y),
                BinOp::Mul => x.checked_mul(y),
                BinOp::Div => x.checked_div(y),
                BinOp::Rem => x.checked_rem(y),
                BinOp::BitAnd => Some(x & y),
                BinOp::BitOr => Some(x | y),
                BinOp::Shl => shift.then(|| ((x as u64) << y) as i64),
                BinOp::Shr => shift.then(|| x >> y),
                _ => return Err(mismatch()),
            };
            match out {
                Some(v) => Ok(Expr { kind: ExprKind::Int(v), ty: Ty::I64 }),
                None => Err("this constant overflows i64, divides by zero or shifts by other than 0..63".to_string()),
            }
        }
        (ExprKind::Float(x), ExprKind::Float(y)) => {
            let (Ty::F64(ux), Ty::F64(uy)) = (&a.ty, &b.ty) else { unreachable!("a float of {:?}", a.ty) };
            let (v, u) = match op {
                BinOp::Mul => (x * y, ux.mul(uy)),
                BinOp::Div => (x / y, ux.div(uy)),
                _ if ux != uy => return Err(mismatch()),
                BinOp::Add => (x + y, ux.clone()),
                BinOp::Sub => (x - y, ux.clone()),
                _ if op.is_comparison() => return boolean(compare(op, x, y)),
                _ => return Err(mismatch()),
            };
            Ok(Expr { kind: ExprKind::Float(v), ty: Ty::F64(u) })
        }
        (ExprKind::Bool(x), ExprKind::Bool(y)) => match op {
            BinOp::And => boolean(*x && *y),
            BinOp::Or => boolean(*x || *y),
            BinOp::Eq => boolean(x == y),
            BinOp::Ne => boolean(x != y),
            _ => Err(mismatch()),
        },
        (ExprKind::Str(x), ExprKind::Str(y)) => match op {
            BinOp::Add => Ok(Expr { kind: ExprKind::Str(format!("{x}{y}")), ty: Ty::Str }),
            _ if op.is_comparison() => boolean(compare(op, x.as_str(), y.as_str())),
            _ => Err(mismatch()),
        },
        _ => Err(mismatch()),
    }
}

fn cmp_op(op: BinOp) -> CmpOp {
    match op {
        BinOp::Eq => CmpOp::Eq,
        BinOp::Ne => CmpOp::Ne,
        BinOp::Lt => CmpOp::Lt,
        BinOp::Le => CmpOp::Le,
        BinOp::Gt => CmpOp::Gt,
        BinOp::Ge => CmpOp::Ge,
        _ => unreachable!("{op:?} is not a comparison"),
    }
}

/// Conservative "every path returns": a return, or an if/else whose branches all return.
fn always_returns(stmts: &[ast::Stmt]) -> bool {
    stmts.iter().any(|s| match &s.kind {
        StmtKind::Return(_) => true,
        // `exit(code)` and `panic(message)` end the program, so nothing after them runs.
        StmtKind::Expr(ast::Expr { kind: AK::Call { callee, .. }, .. }) => {
            callee.name == "exit" || callee.name == "panic"
        }
        StmtKind::Match { arms, .. } => !arms.is_empty() && arms.iter().all(|a| always_returns(&a.body.stmts)),
        StmtKind::If { then, else_: Some(e), .. } => {
            always_returns(&then.stmts)
                && match e.as_ref() {
                    Else::Block(b) => always_returns(&b.stmts),
                    Else::If(st) => always_returns(std::slice::from_ref(st)),
                }
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::{closest, distance};

    #[test]
    fn suggestions() {
        assert_eq!(distance("cuont", "count"), 1);
        assert_eq!(distance("speed", "sped"), 1);
        assert_eq!(distance("abc", "xyz"), 3);
        let names = ["count".to_string(), "total".to_string()];
        assert_eq!(closest("cuont", names.iter()), Some("count".to_string()));
        assert_eq!(closest("zzz", names.iter()), None);
    }
}
