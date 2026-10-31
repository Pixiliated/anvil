//! Values written as Anvil source, for traces: `2.0[m]`, `"text"`, `[1, 2]`, `Some(3)`,
//! `Shape.Rect(width: 2.0[m], height: 3.0[m])`. Long arrays, strings and values are cut, and the
//! text says how much was left out.

use crate::Value;
use anvil_front::ir::{format_f64, Program, Ty};

/// Array elements shown before the rest is summarized.
pub const SHOWN_ITEMS: usize = 8;
/// String bytes shown before the rest is summarized.
pub const SHOWN_BYTES: usize = 80;
/// Characters of a whole value shown before the rest is summarized.
pub const SHOWN_CHARS: usize = 200;

pub fn show(v: &Value, ty: &Ty, program: &Program) -> String {
    let mut out = String::new();
    write(&mut out, v, ty, program);
    if let Some((cut, _)) = out.char_indices().nth(SHOWN_CHARS) {
        let rest = out[cut..].chars().count();
        out.truncate(cut);
        out.push_str(&format!(" ... {rest} more chars"));
    }
    out
}

fn write(out: &mut String, v: &Value, ty: &Ty, program: &Program) {
    match (v, ty) {
        (Value::Void, _) => out.push_str("nothing"),
        (Value::I64(x), _) => out.push_str(&x.to_string()),
        (Value::F64(x), Ty::F64(u)) if !u.is_dimensionless() => out.push_str(&format!("{}[{u}]", format_f64(*x))),
        (Value::F64(x), _) => out.push_str(&format_f64(*x)),
        (Value::Bool(b), _) => out.push_str(&b.to_string()),
        (Value::Str(s), _) => quote(out, s),
        (Value::Struct(fields), Ty::Struct(id)) => {
            let def = &program.structs[*id];
            out.push_str(source_name(&def.name));
            out.push_str(" { ");
            for (i, (value, (name, fty))) in fields.iter().zip(&def.fields).enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(name);
                out.push_str(": ");
                write(out, value, fty, program);
            }
            out.push_str(" }");
        }
        (Value::Array(items), Ty::Array(elem)) => {
            out.push('[');
            for (i, item) in items.iter().take(SHOWN_ITEMS).enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write(out, item, elem, program);
            }
            if items.len() > SHOWN_ITEMS {
                out.push_str(&format!(", ... {} more", items.len() - SHOWN_ITEMS));
            }
            out.push(']');
        }
        (Value::Map(entries), Ty::Map(kt, vt)) => {
            if entries.is_empty() {
                out.push_str("[:]");
                return;
            }
            out.push('[');
            for (i, (k, v)) in entries.iter().take(SHOWN_ITEMS).enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write(out, k, kt, program);
                out.push_str(": ");
                write(out, v, vt, program);
            }
            if entries.len() > SHOWN_ITEMS {
                out.push_str(&format!(", ... {} more", entries.len() - SHOWN_ITEMS));
            }
            out.push(']');
        }
        (Value::Enum(tag, fields), Ty::Enum(id)) => {
            let def = &program.enums[*id];
            let variant = &def.variants[*tag];
            // Option and Result constructors are written without a prefix.
            if !(def.name.starts_with("Option[") || def.name.starts_with("Result[")) {
                out.push_str(source_name(&def.name));
                out.push('.');
            }
            out.push_str(&variant.name);
            if fields.is_empty() {
                return;
            }
            out.push('(');
            let named = fields.len() > 1;
            for (i, (value, (name, fty))) in fields.iter().zip(&variant.fields).enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                if named {
                    out.push_str(name);
                    out.push_str(": ");
                }
                write(out, value, fty, program);
            }
            out.push(')');
        }
        (v, ty) => unreachable!("show {v:?} as {ty:?}"),
    }
}

/// A type's name as source writes it in a literal: `Pair` for the instance `Pair[i64, str]`.
fn source_name(name: &str) -> &str {
    name.split('[').next().unwrap_or(name)
}

fn quote(out: &mut String, s: &str) {
    let mut end = s.len().min(SHOWN_BYTES);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    out.push('"');
    for c in s[..end].chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    if end < s.len() {
        out.push_str(&format!(" ... {} more bytes", s.len() - end));
    }
}
