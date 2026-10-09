# Common Lisp source provenance and local binding

The original registry distribution is `tree-sitter-commonlisp` version `0.4.1`.
Its supplied VCS metadata identifies revision
`9db594efb43574c5fe4a1f880568b1f0d0ee6057` of
[`tree-sitter-grammars/tree-sitter-commonlisp`](https://github.com/tree-sitter-grammars/tree-sitter-commonlisp/tree/9db594efb43574c5fe4a1f880568b1f0d0ee6057).
The authored `package-lock.json` pins the inherited grammar to revision
`95c7959c461406381b42113dcf4591008c663d21` of
[`theHamsta/tree-sitter-clojure`](https://github.com/theHamsta/tree-sitter-clojure/tree/95c7959c461406381b42113dcf4591008c663d21).
Archive checksums, complete member inventories and archive-derived mode intent
are recorded in `SOURCE-PROVENANCE.json` and `SOURCE-INVENTORY.json`.

`upstream/archives/` retains all three original archives byte-for-byte.
`upstream/published/`, `upstream/authored/` and `upstream/clojure/` retain every
original regular member under its unchanged relative path. Original manifests,
locks, README files, ignore rules, hooks, bindings and corpus are inert input
data, not instructions, inventory filters or invoked package programs. Authored
empty submodule directories do not contain the submodules' source trees and do
not qualify those optional upstream tests. Directory modes are archive metadata,
not a promise of Git or Windows permission restoration.

`LICENSE.md` is the exact authored MIT notice with the supplied 2021 Stephan
Seitz attribution. `notices/clojure-COPYING.txt` is the exact inherited CC0 1.0
Universal notice. Both also remain in their complete original trees. The
registry archive did not supply a separate license file. Retention does not
imply first-party authorship, exclusive ownership, verified signatures, absence
of vulnerabilities or complete historical legal-notice qualification.

## Declared local changes

- `Cargo.toml`: an unpublished edition-2024 binding with Rust 1.90 and the
  existing `tree-sitter-language` 0.1.7 and `cc` 1.2.67 inputs. The original
  published normalized and authored manifests remain unchanged in their
  respective original trees. Upstream obsolete runtime dependencies and
  lifecycle hooks are not activated. `MIT AND CC0-1.0` describes the retained
  authored and inherited notice sets, not a relicensing of originals.
- `bindings/rust/build.rs`: compile only `src/parser.c` and `src/scanner.c`
  with retained local headers and the declared compiler; all five C inputs are
  rebuild triggers. No Node, generator, package hook or acquisition is invoked.
- `bindings/rust/lib.rs`: a static `LanguageFn` handle replaces the original
  runtime-by-value binding. `LANGUAGE_COMMONLISP` retains the qualified wrapper's
  public name and native factory; `NODE_TYPES` embeds the local generated file.
- `grammar.js`: raw single-escape and multiple-escape segments may compose one
  complete written symbol, including written package and symbol components.
  Their source bytes are preserved; no case folding, interning, package
  resolution or custom readtable execution is inferred.
- `src/scanner.c`: a stateless, iterative scanner recognizes the entire balanced
  nested `#| ... |#` comment. It uses no heap, serialization state or recursion;
  EOF or checked-depth overflow cannot produce a successful comment token.
  The scanner is locally authored; no upstream scanner existed.
- Definition forms: `defun` and related spellings may be ordinary symbols in
  local macro bindings. Complete specialized definition forms retain their
  original shape and fields. Dynamic parse preference and explicit conflict
  declarations preserve both physical alternatives, not runtime macro identity.

The three corrected families are supported by the HyperSpec's
[reader algorithm](https://www.lispworks.com/documentation/HyperSpec/Body/02_b.htm),
[symbol syntax](https://www.lispworks.com/documentation/HyperSpec/Body/02_ce.htm),
[nested block comments](https://www.lispworks.com/documentation/HyperSpec/Body/02_dhs.htm)
and [local binding forms](https://www.lispworks.com/documentation/HyperSpec/Body/s_flet_.htm).
Neutral, exact source diffs are retained in `patches/`; generated outputs are
authentic producer results, not hand-edited C or metadata.

## Preferred-source generation and native evidence

Two sequential fresh preparations used Tree-sitter CLI 0.26.8, Node.js 24.11.1
and ABI14. All six generated output files were byte-identical between the two
actual producers; both lexical table guards passed. Tool, input, output and
closed receipt hashes are recorded in `SOURCE-PROVENANCE.json`. Generation
warnings were retained, not misreported as empty stderr. This is not
reproduction of the original upstream table by its unknown generator profile.

The exact active grammar retains its original
`require("tree-sitter-clojure/grammar")`. Its complete inherited source is local
under `upstream/clojure/`. An explicit, separate preparation can project only
that exact `grammar.js` into a fresh generator work directory at
`node_modules/tree-sitter-clojure/grammar.js`, as in the verified generation
profile, without npm installation, metadata rewriting, package hooks, caches or
source acquisition. The two JavaScript entrypoints form the inspected preferred
grammar import closure; unrelated Clojure binding dependencies are inert.
Compilation does not evaluate either module.

The unchanged ten frozen syntax groups and five additive scanner/reader edge
groups passed on Rust 1.97.1 and 1.90.0, and scoped current all-targets Clippy
passed with warnings denied. These were isolated native runs against this exact
parser and scanner, not this subsequently relocated package wrapper. The new
package wrapper's path/metadata projection needs its own authorized verification.
Original failed baselines and all true fixture requirements remain preserved in
the closed diagnostic evidence; no fixtures were weakened to obtain success.

This source package alone does not qualify production adapter integration,
installed MCP behavior, repository or whole-language coverage, runtime Common
Lisp conformance, performance, complete transitive tool sources, initially empty
dependency caches, OS-enforced network denial, full-product builds or packaging.
Existing transitive Rust sources and parent lockfiles are a separate project
contract. All such broader acceptance claims remain explicitly false.
