use std::collections::BTreeMap;
use std::fmt;

/// A unit as base-unit exponents, e.g. kg^1 m^1 s^-2. Empty means dimensionless.
/// BTreeMap keeps the order canonical, so equal units compare and print identically.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct Unit(pub BTreeMap<String, i32>);

impl Unit {
    pub fn dimensionless() -> Unit {
        Unit(BTreeMap::new())
    }
    pub fn base(name: &str) -> Unit {
        let mut m = BTreeMap::new();
        m.insert(name.to_string(), 1);
        Unit(m)
    }
    pub fn is_dimensionless(&self) -> bool {
        self.0.is_empty()
    }
    pub fn pow(&self, exp: i32) -> Unit {
        Unit(self.0.iter().map(|(k, v)| (k.clone(), v * exp)).collect())
    }
    pub fn mul(&self, other: &Unit) -> Unit {
        let mut m = self.0.clone();
        for (k, v) in &other.0 {
            let e = m.entry(k.clone()).or_insert(0);
            *e += v;
            if *e == 0 {
                m.remove(k);
            }
        }
        Unit(m)
    }
    pub fn div(&self, other: &Unit) -> Unit {
        self.mul(&other.pow(-1))
    }
    /// Square root; None when any exponent is odd.
    pub fn sqrt(&self) -> Option<Unit> {
        let mut m = BTreeMap::new();
        for (k, v) in &self.0 {
            if v % 2 != 0 {
                return None;
            }
            m.insert(k.clone(), v / 2);
        }
        Some(Unit(m))
    }
}

impl fmt::Display for Unit {
    /// Canonical text: positive exponents joined by `*`, then each negative one as `/x^n`; `1/s` when
    /// there is no numerator. Empty string for dimensionless.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let term = |k: &str, e: i32| if e == 1 { k.to_string() } else { format!("{k}^{e}") };
        let num: Vec<String> = self.0.iter().filter(|(_, e)| **e > 0).map(|(k, e)| term(k, *e)).collect();
        let den: Vec<String> = self.0.iter().filter(|(_, e)| **e < 0).map(|(k, e)| term(k, -*e)).collect();
        if num.is_empty() && den.is_empty() {
            return Ok(());
        }
        if num.is_empty() {
            write!(f, "1")?;
        } else {
            write!(f, "{}", num.join("*"))?;
        }
        for d in den {
            write!(f, "/{d}")?;
        }
        Ok(())
    }
}
