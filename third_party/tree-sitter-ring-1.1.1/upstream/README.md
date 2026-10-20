# tree-sitter-ring

[Tree-sitter](https://tree-sitter.github.io/tree-sitter/) grammar for the [Ring programming language](https://ring-lang.github.io/) (`.ring`, `.rh`, `.rform`).

## Features

- **Full core language** — all three function styles (`func name(params)`, `func name params`, `func name`), all statement terminators (`ok`/`end`/`endif`/`next`/`loop`/`off`/...), classes, packages, anonymous functions, and typed parameters
- **Faithful keyword demotion** — `to`/`in`/`from`/`step` parse as keywords only in `for` bounds and `class ... from` parents, and as identifiers everywhere else, exactly like Ring's own parser
- **Same-line postfix rule** — a call's `(` and a subscript's `[` must sit on the same line as their operand; enforced by the external scanner
- **OOP and brace syntax** — `new`/`new from`, brace initialization, constructor calls, and brace-object access (`obj { ... }`)
- **Rich literals** — string interpolation (`#{...}`), multiline strings, list literals, all comment forms (`//`, `/* */`, `#`), the `^^` power operator
- **Unicode identifiers** — including Arabic
- **Battle-tested** — 147 corpus tests; verified against Ring's own `language/tests/`, `samples/`, and `libraries/` (every syntactically valid, non-DSL file parses)

## Use cases

- **Editor support** — accurate syntax highlighting, code folding, symbol outlines, and navigation in tree-sitter-based editors (Neovim, Helix, Zed, Emacs)
- **Developer tooling** — a ready-made foundation for a Ring formatter, linter, or language server
- **Code intelligence** — structural search-and-replace, semantic diffs, metrics, and documentation generators
- **AI tooling** — code indexing and repo maps that let AI coding assistants understand Ring projects
- **Programmatic parsing** — Node/Python/Rust/Wasm bindings for scripts, doc sites, and playgrounds

## Building and testing

```sh
bun install
bun run build   # tree-sitter generate
bun run test    # run the 147 corpus tests

# Parse any Ring file:
bun run parse yourfile.ring
```

## Known limitations

Ring mutates its own syntax at runtime; a static grammar cannot follow that. The following are unsupported **by design**:

- **NaturalLib natural-language DSL** — commands defined at runtime (`I want window`, ...)
- **`ChangeRingKeyword` / `ChangeRingOperator`** — keywords/operators remapped while the program runs; only remapping onto an already-demotable keyword (e.g. `changeringkeyword in`) parses
- **Counter-guarded keywords** — `ok`/`on`/`off`/`catch`/`done`/`again`/`but`/`else`/`other`/`case` demote to identifiers in expression positions, a superset of Ring's runtime-counter behavior (so e.g. `if ok = 5` parses here although Ring rejects it); `loop`/`exit`/`break`/`continue`/`put`/`get`/`return` never demote — exactly like Ring
- **TypeHints-style declarations** — C-style prefixes such as `public static void func ...`

## License

This project is licensed under the MIT License. See the [`LICENSE`](LICENSE) file for details.