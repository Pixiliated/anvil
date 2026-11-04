//! Inputs for property tests (`test "name" for all xs: [i64] { ... }`): values of any concrete
//! type, from a seeded generator, so the same test sees the same cases on every run; and
//! smaller versions of a failing input, so a failure is reported with a small counterexample.

use crate::Value;
use anvil_front::ir::{Program, Ty, OPTION_NONE};
use std::rc::Rc;

/// SplitMix64: small, fast and the same everywhere.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in 0..n (n > 0).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    fn chance(&mut self, one_in: u64) -> bool {
        self.below(one_in) == 0
    }
}

/// The seed of a property test: its name hashed (FNV-1a), mixed with the run's seed.
pub fn test_seed(name: &str, run_seed: u64) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    h ^ run_seed.wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

const EDGE_INTS: &[i64] = &[0, 1, -1, 2, i64::MAX, i64::MIN];
/// Characters of generated text: mostly ASCII, some that take two or three bytes.
const CHARS: &[char] = &['a', 'b', 'c', 'x', 'y', 'z', 'A', 'Z', '0', '9', ' ', ',', '.', '-', 'é', '漢'];

/// A value of `ty`. `size` bounds lengths and magnitudes; nested values get half, so recursive
/// types (which nest through arrays) end. Arrays have at most 8 items and maps 6, which keeps
/// property tests quick.
pub fn generate(ty: &Ty, program: &Program, rng: &mut Rng, size: u64) -> Value {
    generate_with(ty, program, rng, size, false)
}

/// `generate`, or with `wide`, arrays and maps as long as `size` allows: what measuring how a
/// function's work grows needs.
pub fn generate_with(ty: &Ty, program: &Program, rng: &mut Rng, size: u64, wide: bool) -> Value {
    let generate = |ty: &Ty, rng: &mut Rng, size: u64| generate_with(ty, program, rng, size, wide);
    match ty {
        Ty::I64 => {
            if rng.chance(8) {
                Value::I64(EDGE_INTS[rng.below(EDGE_INTS.len() as u64) as usize])
            } else {
                let span = 10 * size.max(1);
                Value::I64(rng.below(2 * span + 1) as i64 - span as i64)
            }
        }
        // Finite floats in quarters, so counterexamples print briefly.
        Ty::F64(_) => {
            if rng.chance(8) {
                Value::F64([0.0, -0.0, 1.0, -1.0][rng.below(4) as usize])
            } else {
                let span = 4 * size.max(1) as i64;
                Value::F64((rng.below(2 * span as u64 + 1) as i64 - span) as f64 / 4.0)
            }
        }
        Ty::Bool => Value::Bool(rng.chance(2)),
        Ty::Str => {
            let n = rng.below(size + 1);
            let s: String = (0..n).map(|_| CHARS[rng.below(CHARS.len() as u64) as usize]).collect();
            Value::Str(s.into())
        }
        // Mostly small values, sometimes the extremes.
        Ty::Bytes => {
            let n = rng.below(size + 1);
            let b: Vec<u8> = (0..n)
                .map(|_| if rng.chance(4) { [0, 255][rng.below(2) as usize] } else { rng.below(64) as u8 })
                .collect();
            Value::Bytes(b.into())
        }
        Ty::Array(elem) => {
            let n = rng.below(if wide { size } else { size.min(8) } + 1);
            Value::Array(Rc::new((0..n).map(|_| generate(elem, rng, size / 2)).collect()))
        }
        Ty::Map(k, v) => {
            let n = rng.below(if wide { size } else { size.min(6) } + 1);
            let mut entries: Vec<(Value, Value)> = Vec::new();
            for _ in 0..n {
                let key = generate(k, rng, size);
                let value = generate(v, rng, size / 2);
                match crate::find_key(&entries, &key) {
                    Ok(at) => entries[at].1 = value,
                    Err(at) => entries.insert(at, (key, value)),
                }
            }
            Value::Map(Rc::new(entries))
        }
        Ty::Struct(id) => Value::Struct(
            program.structs[*id].fields.iter().map(|(_, t)| generate(t, rng, size / 2)).collect(),
        ),
        Ty::Enum(id) => {
            let variants = &program.enums[*id].variants;
            let tag = rng.below(variants.len() as u64) as usize;
            Value::Enum(tag, variants[tag].fields.iter().map(|(_, t)| generate(t, rng, size / 2)).collect())
        }
        // The checker refuses property tests over functions (E0414).
        Ty::Void | Ty::Fn(_) | Ty::Param(_) | Ty::Error => unreachable!("no generated values of {ty:?}"),
    }
}

/// Smaller versions of `v`, most promising first. Each is strictly simpler, so shrinking ends.
pub fn shrink(v: &Value, ty: &Ty, program: &Program) -> Vec<Value> {
    let mut out = Vec::new();
    match (v, ty) {
        (Value::I64(x), _) => {
            for c in [0, x / 2, x - x.signum()] {
                if c != *x && c.unsigned_abs() < x.unsigned_abs() && !out.contains(&Value::I64(c)) {
                    out.push(Value::I64(c));
                }
            }
        }
        (Value::F64(x), _) => {
            for c in [0.0, x.trunc(), x / 2.0] {
                if c.abs() < x.abs() && !out.contains(&Value::F64(c)) {
                    out.push(Value::F64(c));
                }
            }
        }
        (Value::Bool(true), _) => out.push(Value::Bool(false)),
        (Value::Str(s), _) if !s.is_empty() => {
            let chars: Vec<char> = s.chars().collect();
            out.push(Value::Str("".into()));
            if chars.len() > 1 {
                out.push(Value::Str(chars[..chars.len() / 2].iter().collect::<String>().into()));
                out.push(Value::Str(chars[1..].iter().collect::<String>().into()));
                out.push(Value::Str(chars[..chars.len() - 1].iter().collect::<String>().into()));
            }
            // Simpler characters: everything as `a`.
            let plain: String = chars.iter().map(|_| 'a').collect();
            if plain != **s {
                out.push(Value::Str(plain.into()));
            }
        }
        (Value::Bytes(b), _) if !b.is_empty() => {
            out.push(Value::Bytes(Rc::from(&[][..])));
            if b.len() > 1 {
                out.push(Value::Bytes(b[..b.len() / 2].into()));
                out.push(Value::Bytes(b[1..].into()));
                out.push(Value::Bytes(b[..b.len() - 1].into()));
            }
            if b.iter().any(|x| *x != 0) {
                out.push(Value::Bytes(vec![0u8; b.len()].into()));
            }
        }
        (Value::Array(items), Ty::Array(elem)) if !items.is_empty() => {
            out.push(Value::Array(Rc::new(Vec::new())));
            if items.len() > 1 {
                out.push(Value::Array(Rc::new(items[..items.len() / 2].to_vec())));
            }
            for i in 0..items.len() {
                let mut fewer = (**items).clone();
                fewer.remove(i);
                out.push(Value::Array(Rc::new(fewer)));
            }
            for i in 0..items.len() {
                for smaller in shrink(&items[i], elem, program) {
                    let mut changed = (**items).clone();
                    changed[i] = smaller;
                    out.push(Value::Array(Rc::new(changed)));
                }
            }
        }
        (Value::Map(entries), Ty::Map(_, vt)) if !entries.is_empty() => {
            out.push(Value::Map(Rc::new(Vec::new())));
            for i in 0..entries.len() {
                let mut fewer = (**entries).clone();
                fewer.remove(i);
                out.push(Value::Map(Rc::new(fewer)));
            }
            for i in 0..entries.len() {
                for smaller in shrink(&entries[i].1, vt, program) {
                    let mut changed = (**entries).clone();
                    changed[i].1 = smaller;
                    out.push(Value::Map(Rc::new(changed)));
                }
            }
        }
        (Value::Struct(fields), Ty::Struct(id)) => {
            let types = &program.structs[*id].fields;
            for i in 0..fields.len() {
                for smaller in shrink(&fields[i], &types[i].1, program) {
                    let mut changed = fields.clone();
                    changed[i] = smaller;
                    out.push(Value::Struct(changed));
                }
            }
        }
        (Value::Enum(tag, fields), Ty::Enum(id)) => {
            let def = &program.enums[*id];
            // Option: Some(x) can become None.
            let is_option = def.name.starts_with("Option[");
            if is_option && *tag != OPTION_NONE {
                out.push(Value::Enum(OPTION_NONE, Vec::new()));
            }
            // An earlier variant without fields is simpler.
            for (k, v) in def.variants.iter().enumerate().take(*tag) {
                if v.fields.is_empty() && !(is_option && k == OPTION_NONE) {
                    out.push(Value::Enum(k, Vec::new()));
                }
            }
            let types = &def.variants[*tag].fields;
            for i in 0..fields.len() {
                for smaller in shrink(&fields[i], &types[i].1, program) {
                    let mut changed = fields.clone();
                    changed[i] = smaller;
                    out.push(Value::Enum(*tag, changed));
                }
            }
        }
        _ => {}
    }
    out
}
