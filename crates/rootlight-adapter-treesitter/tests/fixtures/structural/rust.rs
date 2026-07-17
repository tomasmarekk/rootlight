//! Kořenová dokumentace Ω.

use std::fmt;

mod demo {
    /// Pozdraví uživatele.
    pub fn greet(name: &str) -> &'static str {
        let text = "Ahoj 🌍";
        greet(name);
        text
    }
}
