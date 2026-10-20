# Clojure grammar source

This local static binding uses tree-sitter-clojure 0.0.8 at commit
`95c7959c461406381b42113dcf4591008c663d21` from
`https://github.com/theHamsta/tree-sitter-clojure`.
All 16 previously authenticated original files remain under `upstream/`.
`upstream.tar.gz` preserves the exact 47,548-byte archive, SHA-256
`2008d63049215d63920bd11290a83061d4efe0718a3009826479f3926464b6a4`,
including original member modes and directories.

The preferred grammar, generated C/JSON, original parser header and CC0 notice
are byte-identical copies. The generated ABI13 parser has no external scanner.
Only `src/parser.c` and its paired local header are compiler inputs. The local
Rust manifest uses the shared pinned LanguageFn and cc dependencies; it does
not execute the original Tree-sitter 0.19 Rust wrapper, Node/NAN bindings,
npm lifecycle scripts or optional generator commands.

`COPYING.txt` preserves the original CC0 text. Local bindings and Rootlight
analysis do not claim authorship of the retained upstream grammar. The reader
is permissive about some tokens and does not validate compiler, dialect,
reader-conditional selection or macro/runtime semantics. Those remain explicit
analysis gaps; a syntax tree is not proof of complete Clojure support.
