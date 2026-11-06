//! Anvil front end: lexing, parsing, checking. Produces the typed IR both backends consume.

pub mod ast;
pub mod check;
pub mod diag;
pub mod doc;
pub mod fmt;
pub mod ir;
pub mod lexer;
pub mod moves;
pub mod parser;
pub mod span;
pub mod units;

use diag::Diagnostic;
use span::SourceFile;
use std::collections::HashMap;

/// The standard library: modules written in Anvil and built into the compiler, imported as
/// `import std.list.{map};` from any file. Their file names are `std/<name>.anv`.
pub const STD_MODULES: &[(&str, &str)] = &[
    ("expect", include_str!("../std/expect.anv")),
    ("list", include_str!("../std/list.anv")),
    ("math", include_str!("../std/math.anv")),
    ("set", include_str!("../std/set.anv")),
    ("text", include_str!("../std/text.anv")),
];

/// The standard module with a public function of this name.
pub fn std_module_with(name: &str) -> Option<&'static str> {
    let has = |text: &str| text.contains(&format!("pub fn {name}(")) || text.contains(&format!("pub fn {name}["));
    STD_MODULES.iter().find(|(_, text)| has(text)).map(|(n, _)| *n)
}

/// The source of a standard module from its file name (`std/list.anv`).
pub fn std_source(file: &str) -> Option<&'static str> {
    let name = file.strip_prefix("std/")?.strip_suffix(".anv")?;
    STD_MODULES.iter().find(|(n, _)| *n == name).map(|(_, text)| *text)
}

/// Reads a module's source given its file name, which is relative in the same way as the root's
/// name (`tests/programs/lib/shapes.anv` for `import lib.shapes` in `tests/programs/main.anv`).
pub type Load<'a> = &'a dyn Fn(&str) -> Result<String, String>;

/// Source text to checked program, or every error found. Imports find no module; use
/// `compile_with` for a program of several files.
pub fn compile(src: &SourceFile) -> Result<ir::Program, Vec<Diagnostic>> {
    compile_with(src, &no_files).1
}

/// The program rooted at `src`, reading the modules it imports with `load`. Returns the files
/// read (the root first; a diagnostic's `file` indexes this list) and the program or the errors.
pub fn compile_with(src: &SourceFile, load: Load) -> (Vec<SourceFile>, Result<ir::Program, Vec<Diagnostic>>) {
    let (sources, a) = analyze_with(src, load);
    match a.program {
        Some(p) => (sources, Ok(p)),
        None => (sources, Err(a.diags)),
    }
}

/// `compile_with`, with the lines of statements kept for `profile` to count by. Nothing else
/// about the program changes.
pub fn compile_lines(src: &SourceFile, load: Load) -> (Vec<SourceFile>, Result<ir::Program, Vec<Diagnostic>>) {
    let (sources, a) = analyze_lines(src, load, true);
    match a.program {
        Some(p) => (sources, Ok(p)),
        None => (sources, Err(a.diags)),
    }
}

/// Parses `src`; the syntax tree, or the syntax errors (a lexical error stops at once; the
/// parser reports the first error of each item).
pub fn parse(src: &SourceFile) -> Result<ast::File, Vec<Diagnostic>> {
    let toks = lexer::lex(&src.text).map_err(|d| vec![d])?;
    match parser::parse(toks) {
        (file, errors) if errors.is_empty() => Ok(file),
        (_, errors) => Err(errors),
    }
}

/// Everything the checker learns about a one-file program, even with type errors.
pub fn analyze(src: &SourceFile) -> check::Analysis {
    analyze_with(src, &no_files).1
}

/// Everything the checker learns about the program rooted at `src`. Syntax errors in any file
/// stop before checking, so they come back alone with empty facts. Facts are the root's only. A
/// program that checks cleanly must also be in canonical layout (E0110, see `fmt`).
pub fn analyze_with(src: &SourceFile, load: Load) -> (Vec<SourceFile>, check::Analysis) {
    analyze_lines(src, load, false)
}

fn analyze_lines(src: &SourceFile, load: Load, lines: bool) -> (Vec<SourceFile>, check::Analysis) {
    let mut l = Loading {
        load,
        root: src.name.clone(),
        sources: vec![src.clone()],
        asts: Vec::new(),
        names: vec![String::new()],
        imports: vec![Vec::new()],
        c_files: vec![HashMap::new()],
        state: vec![Visit::New],
        stack: Vec::new(),
        order: Vec::new(),
        by_file: HashMap::new(),
        diags: Vec::new(),
    };
    l.by_file.insert(src.name.clone(), 0);
    match parse(src) {
        Ok(f) => l.asts.push(Some(f)),
        Err(d) => {
            l.asts.push(None);
            l.diags.extend(d);
        }
    }
    if l.asts[0].is_some() {
        l.visit(0);
    }
    if !l.diags.is_empty() {
        let a = check::Analysis { program: None, diags: l.diags, facts: check::Facts::default() };
        return (l.sources, a);
    }
    let asts: Vec<ast::File> = l.asts.into_iter().map(|a| a.expect("parsed")).collect();
    // Each file's doc examples become tests of it (run when it is the root).
    let mut examples = Vec::new();
    let mut bad = Vec::new();
    for (i, ast) in asts.iter().enumerate() {
        let (tests, errors) = doc::examples(&l.sources[i], ast);
        examples.push(tests);
        bad.extend(errors.into_iter().map(|mut d| {
            d.file = i;
            d
        }));
    }
    if !bad.is_empty() {
        let a = check::Analysis { program: None, diags: bad, facts: check::Facts::default() };
        return (l.sources, a);
    }
    let modules: Vec<check::Module> = (0..l.sources.len())
        .map(|i| check::Module {
            src: &l.sources[i],
            file: &asts[i],
            name: l.names[i].clone(),
            imports: l.imports[i].clone(),
            c_files: l.c_files[i].clone(),
            examples: &examples[i],
        })
        .collect();
    let a = check::analyze_modules(&modules, &l.order, lines);
    drop(modules);
    (l.sources, a)
}

/// The file that `import shapes.circle` names in the file `importer`: `shapes/circle.anv` in the
/// importer's directory. Every file's imports are relative to itself, so any file can also be
/// checked, tested or run on its own.
pub fn module_file(importer: &str, module: &str) -> String {
    // `std.` is the standard library from any file, wherever it is.
    if let Some(name) = module.strip_prefix("std.") {
        return format!("std/{}.anv", name.replace('.', "/"));
    }
    format!("{}{}.anv", dir_of(importer), module.replace('.', "/"))
}

/// The C file that `extern "lib/x.c"` names in the file `importer`: relative to its directory,
/// like imports.
pub fn c_file(importer: &str, written: &str) -> String {
    format!("{}{written}", dir_of(importer))
}

fn dir_of(file: &str) -> &str {
    match file.rfind('/') {
        Some(i) => &file[..=i],
        None => "",
    }
}

fn no_files(name: &str) -> Result<String, String> {
    Err(format!("no files can be read here (wanted {name}); check the program as part of a project"))
}

type CFiles = HashMap<String, Result<(String, String), String>>;

#[derive(Clone, Copy, PartialEq)]
enum Visit {
    New,
    Active,
    Done,
}

/// Reads and parses every module a program imports, depth first, so `order` lists each module
/// after the ones it imports.
struct Loading<'l> {
    load: Load<'l>,
    root: String,
    sources: Vec<SourceFile>,
    asts: Vec<Option<ast::File>>,
    names: Vec<String>,
    imports: Vec<Vec<usize>>,
    /// Per module, the C files its externs name (see check::Module::c_files).
    c_files: Vec<CFiles>,
    state: Vec<Visit>,
    stack: Vec<usize>,
    order: Vec<usize>,
    /// Files already read, by name; a module is one file however many files import it.
    by_file: HashMap<String, usize>,
    diags: Vec<Diagnostic>,
}

impl Loading<'_> {
    fn visit(&mut self, m: usize) {
        self.state[m] = Visit::Active;
        self.stack.push(m);
        let written: Vec<String> = self.asts[m]
            .iter()
            .flat_map(|f| &f.items)
            .filter_map(|i| match i {
                ast::Item::Fn(f) => f.external.as_ref().and_then(|x| x.source.as_ref()).map(|(w, _)| w.clone()),
                _ => None,
            })
            .collect();
        for w in written {
            let path = c_file(&self.sources[m].name, &w);
            let read = (self.load)(&path).map(|text| (path.clone(), text)).map_err(|e| format!("{path}: {e}"));
            self.c_files[m].insert(w, read);
        }
        let decls: Vec<ast::ImportDecl> = self.asts[m]
            .iter()
            .flat_map(|f| &f.items)
            .filter_map(|i| match i {
                ast::Item::Import(d) => Some(d.clone()),
                _ => None,
            })
            .collect();
        for imp in decls {
            let name = imp.module();
            let file = module_file(&self.sources[m].name, &name);
            let target = match self.by_file.get(&file) {
                Some(t) => *t,
                None => match self.read(file) {
                    Ok(t) => t,
                    Err(why) => {
                        let mut d = Diagnostic::new("E0601", imp.path_span(), format!("cannot find module `{name}`"))
                            .note(why, None);
                        d.file = m;
                        self.diags.push(d);
                        continue;
                    }
                },
            };
            match self.state[target] {
                Visit::Active => {
                    let from = self.stack.iter().position(|s| *s == target).unwrap();
                    let mut chain: Vec<String> =
                        self.stack[from..].iter().map(|s| self.sources[*s].name.clone()).collect();
                    chain.push(self.sources[target].name.clone());
                    let mut d =
                        Diagnostic::new("E0605", imp.path_span(), format!("import cycle: {}", chain.join(" -> ")));
                    d.file = m;
                    self.diags.push(d);
                }
                Visit::New => {
                    if self.asts[target].is_some() {
                        self.visit(target);
                    }
                }
                Visit::Done => {}
            }
            self.imports[m].push(target);
        }
        self.stack.pop();
        self.state[m] = Visit::Done;
        self.order.push(m);
    }

    /// Reads and parses the module in `file`; its index, or why it could not be read.
    fn read(&mut self, file: String) -> Result<usize, String> {
        let text = if file.starts_with("std/") {
            match std_source(&file) {
                Some(text) => text.to_string(),
                None => {
                    let names: Vec<&str> = STD_MODULES.iter().map(|(n, _)| *n).collect();
                    return Err(format!("the standard library has no such module; it has {}", names.join(", ")));
                }
            }
        } else {
            (self.load)(&file).map_err(|e| format!("{file}: {e}"))?
        };
        // Functions of a module are named after its path from the root's directory: `lib.shapes.area`.
        let rel = file.strip_prefix(dir_of(&self.root)).unwrap_or(&file);
        let name = rel.strip_suffix(".anv").unwrap_or(rel).replace('/', ".");
        let src = SourceFile::new(file.clone(), text);
        let index = self.sources.len();
        match parse(&src) {
            Ok(f) => self.asts.push(Some(f)),
            Err(ds) => {
                self.asts.push(None);
                for mut d in ds {
                    d.file = index;
                    self.diags.push(d);
                }
            }
        }
        self.sources.push(src);
        self.names.push(name);
        self.imports.push(Vec::new());
        self.c_files.push(HashMap::new());
        self.state.push(Visit::New);
        self.by_file.insert(file, index);
        Ok(index)
    }
}
