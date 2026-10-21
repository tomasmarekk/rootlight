# tree-sitter-wolfram

A [tree-sitter](https://tree-sitter.github.io/) grammar for the [Wolfram Language](https://www.wolfram.com/language/).

## Supported file types

`.wl`, `.m`, `.wls`, `.wlt`, `.mt`, `.nb`

## Building

`grammar.js` is the source of truth. After any change to it, regenerate the C
parser in `src/` (`parser.c`, `grammar.json`, `node-types.json`):

```bash
npx tree-sitter generate
```

## Testing

Runs the corpus under `test/`:

```bash
npx tree-sitter test
```

## Usage with Zed

This grammar is used by the [zed-wolfram-highlighter](https://github.com/WolframResearch/zed-wolfram-highlighter) extension to provide syntax highlighting in the [Zed](https://zed.dev) editor.

## License

MIT
