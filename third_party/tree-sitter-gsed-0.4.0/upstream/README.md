# tree-sitter-gsed

[![CI](https://github.com/konomanoasa/tree-sitter-gsed/actions/workflows/ci.yaml/badge.svg)](https://github.com/konomanoasa/tree-sitter-gsed/actions/workflows/ci.yaml)
[![crates.io](https://img.shields.io/crates/v/konomanoasa-tree-sitter-gsed)](https://crates.io/crates/konomanoasa-tree-sitter-gsed)
[![npm](https://img.shields.io/npm/v/@konomanoasa/tree-sitter-gsed)](https://www.npmjs.com/package/@konomanoasa/tree-sitter-gsed)

[Tree-sitter](https://tree-sitter.github.io/tree-sitter/) grammars for
GNU sed 4.10.

## Installation

```sh
npm install @konomanoasa/tree-sitter-gsed
```

## Grammars

BRE and ERE have different syntax, so separate grammars are provided.

| Grammar    | Description           |
| ---------- | --------------------- |
| `gsed`     | GNU sed 4.10 with BRE |
| `gsed_ere` | GNU sed 4.10 with ERE |

## Development

Development uses Node.js 24.21.0 or later.

```sh
npm install
npm run build
npm test
```

## Specification

[GNU sed Manual 4.10](https://www.gnu.org/software/sed/manual/sed.html)

## Used By

[sed-ts-mode](https://github.com/konomanoasa/sed-ts-mode): Emacs major mode.

## License

[MIT](LICENSE)
