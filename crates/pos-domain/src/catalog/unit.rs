//! Units of measure (`units` table seed, CAT-007).

use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, strum::EnumIter)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Ea,
    Kg,
    G,
    L,
    Ml,
    M,
    Cm,
    Box,
    Pack,
}

impl Unit {
    pub const fn code(self) -> &'static str {
        match self {
            Unit::Ea => "ea",
            Unit::Kg => "kg",
            Unit::G => "g",
            Unit::L => "l",
            Unit::Ml => "ml",
            Unit::M => "m",
            Unit::Cm => "cm",
            Unit::Box => "box",
            Unit::Pack => "pack",
        }
    }

    /// Whether quantities may be fractional (`0.250 kg`) or must be whole (`3 ea`).
    pub const fn allows_fraction(self) -> bool {
        matches!(self, Unit::Kg | Unit::G | Unit::L | Unit::Ml | Unit::M | Unit::Cm)
    }

    pub fn from_code(code: &str) -> Option<Unit> {
        <Unit as strum::IntoEnumIterator>::iter().find(|u| u.code() == code)
    }
}
