//! Elixir routing uses written suffixes or an exact interpreter identity.
//! Filename mentions and unrelated `env` arguments cannot choose the analyzer.

use super::*;

#[test]
fn elixir_sources_and_scripts_have_explicit_bounded_structural_routes() {
    for path in ["lib/source.ex", "test/check.exs", "MIX.EXS"] {
        assert_eq!(extension_language(path), Some("elixir"));
    }
    for path in ["lib/source.ex.bak", "example.exs.txt", "notex"] {
        assert_ne!(extension_language(path), Some("elixir"));
    }
    let capability = language_capabilities()
        .iter()
        .find(|row| row.language == "elixir")
        .unwrap();
    assert_eq!(capability.maximum_tier, "tier_d");
    assert_eq!(capability.analyzers, &["treesitter"]);
    assert_eq!(canonical_language("elixir"), Some("elixir"));
}

#[test]
fn only_an_executable_elixir_shebang_selects_elixir() {
    for source in [
        b"#!/usr/bin/elixir\n".as_slice(),
        b"#!/usr/bin/env elixir\n",
        b"#!/usr/bin/env -S elixir --no-halt\n",
    ] {
        assert_eq!(shebang_language(source), Some("elixir"));
    }
    for source in [
        b"#!/usr/bin/env -u elixir python\n".as_slice(),
        b"#!/usr/bin/elixir-other\n",
        b"# elixir\n",
    ] {
        assert_ne!(shebang_language(source), Some("elixir"));
    }
}
