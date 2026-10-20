# Pinned D grammar sources

The complete gdamore/tree-sitter-d Git tree and matching archive are retained.
`SOURCE-PROVENANCE.json` binds every original byte, Git mode and archive mode.
Upstream `.gitignore` and `.gitattributes` are inert files with `.upstream` suffixes.

The v0.9.1 tag contains Cargo/npm version 0.8.2; that metadata conflict is retained.
The local package uses version 0.8.2 and unchanged ABI14 C tables/scanner.
Only the local static Rust wrapper is used during compilation. All dependencies
are existing pinned local inputs; builds do not run upstream acquisition or generation.

The scanner is stateless and retains its original 256-character heredoc delimiter
limit. Native syntax controls are component evidence rather than whole-language
or installed-product qualification. MIT license text and original source headers
remain unchanged.
