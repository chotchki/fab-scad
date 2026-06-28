//! A TOML scalar that may be written as either an integer or a float — chotchki hand-edits
//! these manifests, so `at = -10` and `at = -10.0` must both parse.

use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    pub fn f(self) -> f64 {
        match self {
            Num::Int(i) => i as f64,
            Num::Float(x) => x,
        }
    }
}

impl Default for Num {
    fn default() -> Self {
        Num::Float(0.0)
    }
}
