## tree-sitter-coffeescript

Tree-sitter grammar for CoffeeScript. This repository contains the grammar (`grammar.js`), generated parser sources, and queries for syntax highlighting and other editor features.

Overview
- Target language: CoffeeScript
- Outputs: C parser (for Tree-sitter), Node and Rust bindings
- Queries: see `queries/` for highlights, injections, folds

Quick Start
Prereqs:
- Node.js (for `tree-sitter` CLI and Node bindings)
- Rust toolchain (for the Rust crate)

Install and generate:
- `npm install`
- `npx tree-sitter generate`

Run tests:
- `npm test`

Development
- Edit `grammar.js`, then regenerate: `npx tree-sitter generate`
- Update queries under `queries/*.scm`
- Iterate with tests: `npm test`

Project layout
- `grammar.js` — CoffeeScript grammar definition
- `src/` — generated C parser sources and scanner (if any)
- `queries/` — Tree-sitter query files for highlights, injections, folds
- `bindings/` — Node and Rust binding glue
- `test/` — language samples and expectations for `tree-sitter test`
