//! Typed IR produced by the checker. Names are resolved, call arguments are in parameter order,
//! and every operation that can fail carries its finished panic message, so the interpreter and
//! the native backend cannot disagree about what a failure says.

use crate::units::Unit;
use std::fmt;

pub type LocalId = usize;
pub type FuncId = usize;
pub type StructId = usize;
pub type EnumId = usize;

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Void,
    I64,
    F64(Unit),
    Bool,
    Str,
    Struct(StructId),
    /// `[T]`: a growable array with value semantics (copied on write).
    Array(Box<Ty>),
    Enum(EnumId),
    /// Produced after an error so one mistake does not cascade; never reaches a backend.
    Error,
}

#[derive(Clone, Debug)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<(String, Ty)>,
}

/// A user enum, or one instantiation of the built-in `Option[T]` / `Result[T, E]`.
#[derive(Clone, Debug)]
pub struct EnumDef {
    /// As shown to users: `Shape`, `Option[i64]`.
    pub name: String,
    pub variants: Vec<VariantDef>,
}

#[derive(Clone, Debug)]
pub struct VariantDef {
    pub name: String,
    pub fields: Vec<(String, Ty)>,
}

/// Variant order of the built-in enums: the success variant is always 0, the failure 1.
pub const OPTION_SOME: usize = 0;
pub const OPTION_NONE: usize = 1;
pub const RESULT_OK: usize = 0;
pub const RESULT_ERR: usize = 1;

#[derive(Clone, Debug)]
pub struct Local {
    pub name: String,
    pub ty: Ty,
}

/// A runtime check (requires, ensures) with its complete panic message.
#[derive(Clone, Debug)]
pub struct Check {
    pub expr: Expr,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct Func {
    pub name: String,
    pub params: Vec<LocalId>,
    pub ret: Ty,
    pub uses_io: bool,
    pub requires: Vec<Check>,
    pub ensures: Vec<Check>,
    /// Bound to the return value while ensures clauses run.
    pub result_local: Option<LocalId>,
    pub locals: Vec<Local>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct Test {
    pub name: String,
    pub uses_io: bool,
    pub locals: Vec<Local>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct Program {
    pub structs: Vec<StructDef>,
    pub enums: Vec<EnumDef>,
    pub funcs: Vec<Func>,
    pub tests: Vec<Test>,
    pub main: Option<FuncId>,
}

/// One step from a local to the place being assigned or mutated.
#[derive(Clone, Debug)]
pub enum Step {
    Field(usize),
    /// Bounds-checked; `site` is the source location named by the panic.
    Index {
        index: Expr,
        site: String,
    },
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub variant: usize,
    /// (field index in the variant, local bound to it)
    pub bindings: Vec<(usize, LocalId)>,
    pub body: Vec<Stmt>,
}

/// Evaluation order, shared by both backends: the right-hand value first, then the index
/// expressions of the place from left to right, then bounds checks as the place is reached.
#[derive(Clone, Debug)]
pub enum Stmt {
    Let(LocalId, Expr),
    /// Assign to a local or a place inside it.
    Assign(LocalId, Vec<Step>, Expr),
    Expr(Expr),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    While(Expr, Vec<Stmt>),
    /// `for i in start..end`, end exclusive, both evaluated once.
    For(LocalId, Expr, Expr, Vec<Stmt>),
    /// `for x in array`: iterates a snapshot held in the hidden local `held`.
    ForEach {
        var: LocalId,
        held: LocalId,
        iter: Expr,
        body: Vec<Stmt>,
    },
    /// The scrutinee is stored in the hidden local `held`; exactly one arm per variant.
    Match {
        held: LocalId,
        scrutinee: Expr,
        arms: Vec<Arm>,
    },
    Return(Option<Expr>),
    Break,
    Continue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug)]
pub enum Builtin {
    /// Prints each argument (unit-carrying floats with their unit) then a newline.
    Print,
    Assert {
        message: String,
    },
    Sqrt,
    AbsI64 {
        message: String,
    },
    AbsF64,
    ToF64,
    ToI64 {
        message: String,
    },
    /// Length of a str (bytes) or an array (elements).
    Len,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: Ty,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Local(LocalId),
    Field(Box<Expr>, usize),
    Call(FuncId, Vec<Expr>),
    StructLit(StructId, Vec<Expr>),
    ArrayLit(Vec<Expr>),
    /// Construct `variant` of an enum; `fields` are that variant's fields in order.
    Variant {
        enum_id: EnumId,
        variant: usize,
        fields: Vec<Expr>,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
        site: String,
    },
    /// Concatenation of the text of each part (str, i64, f64 with its unit, bool).
    Interp(Vec<Expr>),
    StrConcat(Box<Expr>, Box<Expr>),
    /// `xs.push(v)` on the array at a place.
    Push {
        local: LocalId,
        path: Vec<Step>,
        value: Box<Expr>,
    },
    /// `xs.pop()`: removes and returns the last element; panics with `message` when empty.
    Pop {
        local: LocalId,
        path: Vec<Step>,
        message: String,
    },
    /// `e?` on Option/Result: the success payload, or return the failure from the function.
    /// The value is first stored in the hidden local `held`; `ret_enum` is the function's return type.
    Try {
        held: LocalId,
        inner: Box<Expr>,
        ret_enum: EnumId,
    },
    /// Checked integer arithmetic. `overflow` and `div_zero` are the panic messages.
    IArith {
        op: IOp,
        l: Box<Expr>,
        r: Box<Expr>,
        overflow: String,
        div_zero: String,
    },
    FArith(FOp, Box<Expr>, Box<Expr>),
    Cmp(CmpOp, Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    INeg {
        e: Box<Expr>,
        overflow: String,
    },
    FNeg(Box<Expr>),
    Builtin(Builtin, Vec<Expr>),
}

// ---- shared runtime semantics: both backends must use these ----

pub const PANIC_EXIT_CODE: i32 = 101;

pub fn overflow_message(op: &str, site: &str) -> String {
    format!("integer overflow in `{op}` at {site}")
}
pub fn div_zero_message(site: &str) -> String {
    format!("division by zero at {site}")
}
pub fn main_exit_message(code: i64) -> String {
    format!("main returned {code}, outside the exit code range 0..=255")
}
/// Mirrors anvil_panic_index in the C runtime.
pub fn index_message(index: i64, len: i64, site: &str) -> String {
    format!("index {index} out of bounds for length {len} at {site}")
}
pub fn pop_message(site: &str) -> String {
    format!("pop from an empty array at {site}")
}

/// i64 -> f64 range limits for to_i64: values must lie in [-2^63, 2^63).
pub const TO_I64_MIN: f64 = -9223372036854775808.0;
pub const TO_I64_MAX_EXCLUSIVE: f64 = 9223372036854775808.0;

/// Anvil's float text: shortest round-trip digits; positional when the decimal exponent is in
/// [-5, 17), scientific otherwise; always contains a `.`. Mirrors anvil_format_f64 in the C runtime.
pub fn format_f64(v: f64) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v < 0.0 { "-inf".into() } else { "inf".into() };
    }
    let mut sci = String::new();
    for prec in 1..=17usize {
        sci = format!("{:.*e}", prec - 1, v);
        if sci.parse::<f64>().unwrap() == v {
            break;
        }
    }
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp10: i32 = exp.parse().unwrap();
    let neg = mant.starts_with('-');
    let mut digits: Vec<u8> = mant.bytes().filter(|b| b.is_ascii_digit()).collect();
    while digits.len() > 1 && *digits.last().unwrap() == b'0' {
        digits.pop();
    }
    let nd = digits.len() as i32;
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if (-5..17).contains(&exp10) {
        if exp10 < 0 {
            out.push_str("0.");
            for _ in 0..(-exp10 - 1) {
                out.push('0');
            }
            out.push_str(std::str::from_utf8(&digits).unwrap());
        } else {
            let mut i = 0;
            while i <= exp10 {
                out.push(if i < nd { digits[i as usize] as char } else { '0' });
                i += 1;
            }
            out.push('.');
            if i >= nd {
                out.push('0');
            }
            while i < nd {
                out.push(digits[i as usize] as char);
                i += 1;
            }
        }
    } else {
        out.push(digits[0] as char);
        out.push('.');
        if nd == 1 {
            out.push('0');
        }
        out.push_str(std::str::from_utf8(&digits[1..]).unwrap());
        out.push_str(&format!("e{exp10}"));
    }
    out
}

pub struct TyDisplay<'a> {
    pub ty: &'a Ty,
    pub structs: &'a [StructDef],
    pub enums: &'a [EnumDef],
}

impl fmt::Display for TyDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.ty {
            Ty::Void => write!(f, "nothing"),
            Ty::I64 => write!(f, "i64"),
            Ty::F64(u) if u.is_dimensionless() => write!(f, "f64"),
            Ty::F64(u) => write!(f, "f64[{u}]"),
            Ty::Bool => write!(f, "bool"),
            Ty::Str => write!(f, "str"),
            Ty::Struct(id) => write!(f, "{}", self.structs[*id].name),
            Ty::Array(t) => write!(f, "[{}]", TyDisplay { ty: t, structs: self.structs, enums: self.enums }),
            Ty::Enum(id) => write!(f, "{}", self.enums[*id].name),
            Ty::Error => write!(f, "<error>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::format_f64;

    #[test]
    fn float_text() {
        let cases = [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (3.5, "3.5"),
            (0.1, "0.1"),
            (100.0, "100.0"),
            (1e16, "10000000000000000.0"),
            (1e17, "1.0e17"),
            (1.5e-5, "0.000015"),
            (1.5e-6, "1.5e-6"),
            (123456.789, "123456.789"),
            (f64::MAX, "1.7976931348623157e308"),
            (5e-324, "5.0e-324"),
            (2.0f64.sqrt(), "1.4142135623730951"),
        ];
        for (v, s) in cases {
            assert_eq!(format_f64(v), s, "formatting {v:e}");
        }
    }
}
