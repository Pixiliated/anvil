//! Syntax tree straight from the parser. The checker turns it into the typed IR in `ir.rs`.

use crate::span::Span;

#[derive(Clone, Debug)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct File {
    pub items: Vec<Item>,
}

#[derive(Clone, Debug)]
pub enum Item {
    Import(ImportDecl),
    Unit(UnitDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Fn(Box<FnDecl>),
    Test(TestDecl),
}

/// `import shapes.circle.{area, Circle};`: names from the module in `shapes/circle.anv`, a path
/// relative to the directory of the importing file.
#[derive(Clone, Debug)]
pub struct ImportDecl {
    pub path: Vec<Ident>,
    pub names: Vec<Ident>,
    pub span: Span,
}

impl ImportDecl {
    /// `shapes.circle`
    pub fn module(&self) -> String {
        self.path.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(".")
    }
    pub fn path_span(&self) -> Span {
        self.path[0].span.to(self.path.last().unwrap().span)
    }
}

/// `T: ordered + text` in `fn max[T: ordered + text]`: a type parameter and its constraints.
#[derive(Clone, Debug)]
pub struct TypeParamDecl {
    pub name: Ident,
    pub constraints: Vec<Ident>,
}

/// `unit m` declares a base unit; `unit N = kg*m/s^2` an alias.
#[derive(Clone, Debug)]
pub struct UnitDecl {
    pub public: bool,
    pub name: Ident,
    pub def: Option<UnitExpr>,
    pub span: Span,
}

/// Product of unit names with signed exponents; `/` is already folded into negative exponents.
#[derive(Clone, Debug)]
pub struct UnitExpr {
    pub terms: Vec<(Ident, i32)>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct TypeExpr {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum TypeKind {
    /// `f64[m/s]` carries a unit; `Option[i64]` carries type arguments; the parser decides by name.
    Named { name: Ident, unit: Option<UnitExpr>, args: Vec<TypeExpr> },
    /// `[T]`
    Array(Box<TypeExpr>),
}

#[derive(Clone, Debug)]
pub struct StructDecl {
    pub public: bool,
    pub name: Ident,
    pub type_params: Vec<TypeParamDecl>,
    pub fields: Vec<(Ident, TypeExpr)>,
    pub span: Span,
}

/// `enum Shape { Circle(radius: f64[m]), Empty }`
#[derive(Clone, Debug)]
pub struct EnumDecl {
    pub public: bool,
    pub name: Ident,
    pub type_params: Vec<TypeParamDecl>,
    pub variants: Vec<VariantDecl>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct VariantDecl {
    pub name: Ident,
    pub fields: Vec<(Ident, TypeExpr)>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: Ident,
    pub ty: TypeExpr,
}

#[derive(Clone, Debug)]
pub struct FnDecl {
    pub public: bool,
    pub name: Ident,
    pub type_params: Vec<TypeParamDecl>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub uses: Vec<Ident>,
    /// Span of the whole `uses io` clause, for removal fixes.
    pub uses_span: Option<Span>,
    /// Where `uses io` would be inserted.
    pub uses_insert: Span,
    pub requires: Vec<Expr>,
    pub ensures: Vec<Expr>,
    pub body: Block,
    pub span: Span,
    /// `allow LINT "reason"` lines written before `fn`.
    pub allows: Vec<AllowDecl>,
}

/// `allow untested "reason"`: switches one lint off for one function, saying why.
#[derive(Clone, Debug)]
pub struct AllowDecl {
    pub lint: Ident,
    pub reason: String,
    pub reason_span: Span,
    /// The whole line, for removal fixes.
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct TestDecl {
    pub name: String,
    pub name_span: Span,
    pub uses: Vec<Ident>,
    pub uses_span: Option<Span>,
    pub uses_insert: Span,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    Let { mutable: bool, kw_span: Span, name: Ident, ty: TypeExpr, init: Expr },
    Assign { target: Expr, value: Expr },
    Expr(Expr),
    If { cond: Expr, then: Block, else_: Option<Box<Else>> },
    While { cond: Expr, body: Block },
    For { var: Ident, start: Expr, end: Expr, body: Block },
    ForEach { var: Ident, iter: Expr, body: Block },
    Match { scrutinee: Expr, arms: Vec<Arm>, span_end: Span },
    Return(Option<Expr>),
    Break,
    Continue,
}

/// `Rect(width, height: h) => { ... }`
#[derive(Clone, Debug)]
pub struct Arm {
    pub variant: Ident,
    /// None for `Empty`; Some for `Circle(...)`.
    pub fields: Option<Vec<PatField>>,
    pub fields_span: Span,
    pub body: Block,
    pub span: Span,
}

/// `field` binds the field to its own name; `field: name` or `field: _` renames or ignores it.
#[derive(Clone, Debug)]
pub struct PatField {
    pub field: Ident,
    pub binding: Option<Ident>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Else {
    Block(Block),
    If(Box<Stmt>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

impl BinOp {
    pub fn text(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
        }
    }
    pub fn is_comparison(self) -> bool {
        matches!(self, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Debug)]
pub struct Arg {
    pub name: Option<Ident>,
    pub value: Expr,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int(i64),
    Float(f64, Option<UnitExpr>),
    Bool(bool),
    Str(String),
    Name(Ident),
    Result,
    Field(Box<Expr>, Ident),
    Call {
        callee: Ident,
        args: Vec<Arg>,
    },
    Method {
        receiver: Box<Expr>,
        method: Ident,
        args: Vec<Arg>,
    },
    Index(Box<Expr>, Box<Expr>),
    /// `xs[from..to]`
    Slice(Box<Expr>, Box<Expr>, Box<Expr>),
    Array(Vec<Expr>),
    /// `[key: value, ...]`; `[:]` is the empty map.
    MapLit(Vec<(Expr, Expr)>),
    Interp(Vec<InterpPart>),
    Try(Box<Expr>),
    StructLit {
        name: Ident,
        fields: Vec<(Ident, Expr)>,
    },
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Paren(Box<Expr>),
}

#[derive(Clone, Debug)]
pub enum InterpPart {
    Lit(String),
    Expr(Expr),
}
