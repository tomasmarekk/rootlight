# Local operational derivative

The complete CUDA tree at `d58080a327756e4d1d16ec329ba7cb2048f6c6cd`
remains byte exact under `upstream/`. Its generated ABI15 parser tables, grammar,
node inventory, private headers, upstream metadata and original bindings are
unchanged. The exact C++ generation commit `8b5b49eb196bec7040441bee33b2c9a4838d6967`,
the separate historical gitlink `a71474021410973b29bfe99440d57bcd750246b1` and
published C 0.24.1 files remain under separate `upstream-dependencies/` directories.
The four complete archives and original manifests are retained as provenance.

The active scanner uses sixteen `uint32_t` delimiter scalars. A canonical versioned
wire contains a four-byte header plus zero to sixteen little-endian scalars, with
exact bounds of 4 to 68 bytes. Restore, serialization and scanner trials are
transactional; strict refusal remains outside snapshots. Invalid masks and false
trials preserve payload and result symbol. Complete sixteen-scalar delimiters fit;
a seventeenth scalar refuses capacity without truncation. Embedded NUL content is
distinguished from EOF through the native lexer. Scanner steps measure finite
logical work, not total parser work, elapsed time, native heap or RSS.

The owned Rust host preserves the existing synchronous, thread-affine Rc callback
lifecycle and native detach-before-release contract. Each callback retains its own
temporary owner before invoking user code. Callback panic cannot cross the C ABI.
The one-element Vec reserves fallibly; stable Rc owner allocation remains infallible
and provides no hard memory-accounting guarantee. Eight lifecycle tests are included
but have not been executed for this package. The build script compiles local parser,
scanner and status sources only, using exact existing `cc = 1.2.67` and
`tree-sitter-language = 0.1.7` inputs. It runs no upstream generator or installer.

Earlier isolated C evidence covered twelve commands, twenty scanner controls and
two complete original CUDA fixtures. That historical source frame is preserved in
the ignored development archive. This relocated package has not been compiled or
executed. Source conservation alone does not qualify the Rust host, product parser,
cache/pool integration, encoded source admission or structural/semantic indexing.
No discovery, language routing, runtime registration, source policy or production
admission is introduced by this package.

## Git preservation of original CRLF inputs

Two local descendant `.gitattributes` files retain the exact published bytes of
`upstream/test/corpus/c/crlf.txt` and
`upstream-dependencies/cpp-generation/test/corpus/c/crlf.txt`. The parent upstream
attributes, README files and all 227 upstream inputs remain unchanged. These
overrides, local inventory and provenance changes do not alter compiled scanner,
parser, status bridge or owned host bytes and do not requalify the product.
