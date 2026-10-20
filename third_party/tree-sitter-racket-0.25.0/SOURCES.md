# Retained Racket syntax inputs

The upstream source is `6cdh/tree-sitter-racket`, tag `v0.25.0`, revision
`b3cbbde75ce2de3079e33c7fe90bd41b1d45be48`. Its complete archive and all 71
regular members are retained under `upstream/`. `SOURCE-PROVENANCE.json`
binds each original file's content and executable intent. Upstream Git ignore
and attributes files use an inert `.upstream` suffix; their bytes are unchanged.
The original MIT license is retained in `LICENSE` and `upstream/LICENSE`.

The generated ABI15 parser, node metadata and grammar are unchanged. The local
Rust binding builds only the retained parser and scanner as C11, using the
workspace's existing `cc` and `tree-sitter-language` inputs. It does not invoke
upstream package scripts, regenerate syntax tables or acquire dependencies.

`patches/scalar-delimiter-comparison.patch` changes here-string delimiter
comparison from C-string equality to equal-length scalar-byte equality. The
scanner stores `int32_t` characters, so embedded zero bytes must participate in
comparison. The complete original scanner remains in `upstream/src/scanner.c`.
Native callback controls cover delimiters, Unicode, allocation cleanup and
stateless serialization; Rust tests additionally exercise the real parser.

This package supplies default-reader syntax. It does not implement custom
`#lang` readers, macro expansion, module loading or runtime binding semantics.
