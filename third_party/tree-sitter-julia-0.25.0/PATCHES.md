# Retained Julia source modifications

The exact `tree-sitter-julia` 0.25.0 complete source release and authored revision
`e0f9dcd180fdcfcfa8d79a3531e11d99e79321d3` remain unchanged under `upstream/`.
Both original archives and all 61 files from each inventory retain authenticated
bytes and archive-derived mode intent. The two inventories contain identical
file bytes. Original attribute and ignore files use `.gitattributes.upstream`
and `.gitignore.upstream` so they cannot alter this repository's source retention
policy. Archive modes remain recorded independently of Windows file permissions.

The preferred grammar, generated parser, scanner, node schema and support
headers are literal upstream inputs. No grammar or generated table is hand
edited; the parser's original compiler optimization pragmas remain unchanged.
Native cost measurements and any explicit optimization overlay are separate
from authenticating the upstream syntax inputs.

`patches/local-binding.patch` describes the unpublished local Rust manifest and
binding. It pins already versioned `tree-sitter-language` 0.1.7 and `cc` 1.2.67,
declares Rust 1.90, and compiles only retained C11 parser/scanner inputs using
the declared compiler. Upstream dependency locks, generation scripts, npm
lifecycle hooks and repository programs are never invoked during compilation.
The static ABI15 handle remains subject to runtime compatibility checks.

Native syntax, scanner-content, encoding, cancellation and edit tests establish
finite source behavior. They do not establish whole-language corpus coverage,
declaration extraction, lexical or import binding, macro expansion, runtime
dispatch, installed MCP behavior, or offline package acceptance. Each remains
an independent validation obligation.
