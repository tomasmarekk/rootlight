//! Root-level documentation Ω.

use std::fmt;

mod demo {
    /// Greets the user.
    pub fn greet(name: &str) -> &'static str {
        let text = "Hello 🌍";
        greet(name);
        text
    }
}
