# Local SPARQL derivation

This package retains the complete MIT source tree and archive at upstream commit
`1ef52d35a73a2a5f2e433ecfd1c751c1360a923b`. Original manifests, bindings,
tests and notices remain inert under `upstream/`.

The upstream Node and Tree-sitter manifests declare version `0.1.0`; the original
Cargo manifest declares `0.0.1`, edition2018 and Tree-sitter0.17. The local binding
uses version0.1.0, edition2024 and the repository's reviewed retained dependencies.
No upstream lifecycle script is executed.

The preferred `grammar.js` admits empty CONSTRUCT templates and prologue-only
empty update requests. Its update branch remains nonempty inside the repeated
file unit; the recursive semicolon tail may be empty. Right association keeps
adjacent BASE/PREFIX declarations together and prefers continuing a prologue
into its following query or update operation. The CONSTRUCT where clause remains
required, with its existing optional WHERE keyword.

These forms follow rules 17, 29 and 73 of the
[SPARQL 1.1 grammar](https://www.w3.org/TR/sparql11-query/#sparqlGrammar).
Malformed braces, incomplete prologues, trailing junk and repeated semicolons
remain native recovery cases. The unchanged upstream grammar and generated
Tree-sitter 0.25.3 parser remain under `upstream/files/`.

Generation is a separate maintainer preparation step using the retained
Tree-sitter CLI 0.26.8 and its bundled native QuickJS runtime. Run from this
package root so the local `tree-sitter.json` supplies the ABI 15 metadata:

```text
tree-sitter generate --js-runtime native --abi 15 grammar.js
```

`SOURCE-PROVENANCE.json` binds the generator, preferred grammar, configuration and
generated outputs by hashes. Compilation uses only the retained local C11 parser
and headers; the build script performs no acquisition, generation, lifecycle
script or package-manager invocation.

The grammar accepts query and update syntax, including multiple units. Parser
loading, syntax recovery and physical-span tests do not prove SPARQL evaluation,
semantic validity, production discovery or structural/semantic indexing. In
particular, upstream syntax admits variables in INSERT DATA; semantic checks are
outside this intake. Original corpus witnesses are read in place by opt-in tests.
