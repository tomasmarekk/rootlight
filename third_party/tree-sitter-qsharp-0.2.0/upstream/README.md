# tree-sitter-qsharp

A [tree-sitter](https://tree-sitter.github.io/) grammar for [Q#](https://learn.microsoft.com/en-us/azure/quantum/overview-what-is-qsharp-and-qdk), Microsoft's quantum programming language.

This is a new project, not affiliated with Microsoft or the tree-sitter organization. Feedback, bug reports, and contributions are welcome.

## What it does

Parses Q# source files into a concrete syntax tree that editors can use for syntax highlighting, code folding, symbol navigation, and indentation.

Built by reading the [Q# language reference](https://learn.microsoft.com/en-us/azure/quantum/user-guide/) and the [Q# compiler source](https://github.com/microsoft/qsharp). Tested against Q# files in the microsoft/qsharp repository.

### Supported constructs

- Namespaces, imports, exports, open directives
- Operations and functions with type parameters, functor clauses, specializations
- Structs, newtypes, all builtin types
- Full expression grammar with operator precedence per the [official specification](https://learn.microsoft.com/en-us/azure/quantum/user-guide/language/expressions/precedenceandassociativity)
- Quantum-specific syntax: `use`/`borrow`, `within`/`apply`, `repeat`/`until`/`fixup`, `Adjoint`/`Controlled`
- All literals: int, bigint, float, imaginary, string, interpolated string, bool, Result, Pauli
- External scanner for float/range disambiguation (`1.` vs `0..5`)

### Known limitations

- Explicit type arguments on calls (`Foo<Int>(x)`): the current Q# compiler parses `<` only as the comparison operator, so it reads `Foo<Int>(x)` as a comparison chain rather than a type application — this grammar matches that behavior. The syntax appears in older Q# docs but is not accepted by the current compiler (verified by running it).
- Open-ended step ranges (`...2...`) parse but the AST does not perfectly represent all range component positions.
- This is a v0.x project. There may be edge cases that are not yet covered.

## Editor query files

| Feature | File |
|---|---|
| Syntax highlighting | `queries/highlights.scm` |
| Code folding | `queries/folds.scm` |
| Symbol outline | `queries/tags.scm` |
| Auto-indentation | `queries/indents.scm` |
| Scope tracking | `queries/locals.scm` |
| Text objects | `queries/textobjects.scm` |

Capture names follow the [nvim-treesitter](https://github.com/nvim-treesitter/nvim-treesitter/blob/main/CONTRIBUTING.md) conventions.

## Contributing

If you find Q# code that doesn't parse correctly, please open an issue with the `.qs` file or a minimal reproducing snippet. Pull requests are welcome.

## References

- [Q# overview](https://learn.microsoft.com/en-us/azure/quantum/overview-what-is-qsharp-and-qdk)
- [microsoft/qsharp](https://github.com/microsoft/qsharp) — Q# compiler and standard library (MIT)
- [tree-sitter](https://tree-sitter.github.io/tree-sitter/) — parser generator

## License

MIT
