# ERB delimiter profile

The preferred `grammar.js` starts from upstream revision
`c70c1de07dedd532089c0c90835c8ed9fa694f5c`. Complete original inputs,
archives, license and executable intent remain under `upstream/` and in
`SOURCE-INVENTORY.json`; `erb-delimiters.patch` records the generator-input diff.

The shared upstream grammar accepts several template dialects. This local profile
treats `<%-` as an ERB statement with left trimming, preserves `_`, `~` and `|`
after an ordinary `<%` opener, and preserves `_` or `=` before an ordinary `%>`
closer. It retains `<%=` and Rails `<%==` output tags, comments, escaped `<%%`
content, and explicit `-%>` closes. A Ruby call beginning with `graphql` remains
Ruby source instead of selecting a GraphQL host directive. Invalid authored Ruby
is retained for independent native Ruby recovery rather than made valid by
discarding foreign delimiter characters.

The exact upstream `tree-sitter.json` is also retained as active generator
configuration because the pinned generator otherwise falls back to ABI14. Its
published injection metadata does not activate a Rootlight production route.

Regeneration uses the authenticated Tree-sitter CLI 0.26.8 maintainer executable,
SHA-256 `a61249a07c4a6d01d0bbbb5b7c058e29e12c119ef7fb5bede0b7fd916a7c56f0`.
From this grammar directory, run:

```text
tree-sitter generate --js-runtime native --abi 15
```

The native JavaScript runtime is bundled in that declared maintainer tool.
Generation is separate from Cargo compilation; builds consume reviewed generated
sources and never invoke a generator or acquire code. Generated C and headers
are not hand edited. The generated parser's ABI15 declaration is checked before
native compilation.

The native regressions cover delimiter roles, exact Ruby code ranges, combined
cross-tag blocks, source recovery for foreign markers, comments and escaped
future templates. They do not qualify runtime rendering, escaping, helper
dispatch, or a production ERB route. Empty code ranges still select no Ruby child.

Delimiter references:

- [Ruby ERB compiler trim and tag handling](https://github.com/ruby/erb/blob/master/lib/erb/compiler.rb)
- [EJS tag reference](https://ejs.co/docs/#tags)
