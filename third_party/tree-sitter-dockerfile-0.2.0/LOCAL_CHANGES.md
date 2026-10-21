# Local Dockerfile inputs

All 75 source leaves from camdencheek/tree-sitter-dockerfile commit
971acdd908568b4531b0ba28a445bf0bb720aba5 remain byte-exact under upstream/.
Upstream .gitignore/.gitattributes use inert .upstream names. No executable
intent was declared by the authenticated archive inventory.

The root MIT license and upstream Cargo MIT declaration are retained. Upstream
package.json and tree-sitter.json declare ISC; this conflicting inert metadata
is preserved rather than rewritten or represented as a new dual license grant.

generated/ contains the reviewed three-rule CRLF grammar derivative and the
actual Root-generated ABI-14 parser, JSON and header outputs from Tree-sitter
CLI 0.26.8 with its native JavaScript runtime. active-src/ holds the separately
reviewed bounded scanner and sticky status bridge. The local Rust wrapper owns
fallibly allocated cancellation storage and closes callbacks before dropping it.

Compilation uses only retained C11 sources and pinned local cc/language crates.
Native six-case success predates this wrapper and does not qualify production,
an installed package, or offline self-containment. Native memory enforcement
remains Unavailable; fixed scanner state is not a hard parser memory guarantee.

Callback ownership correction: every callback owns an independent standard Rc
count before invoking user code. The C-held raw count is paired only after detach.
Same-thread finish/poll/begin reentry stays a sticky conflict; a self-dropped guard
leaves its worker occupied until thread exit. Callback and final context destructor
panics are contained under Rust's unwind configuration and reported through native
status without reading destroyed data. Panic payloads are deliberately leaked.
The Vec's generic storage remains fallibly reserved; standard Rc adds an explicit
infallible constant-size owner allocation. Native memory enforcement is still
Unavailable. The old unsafe predecessor is preserved and must not be executed as
a normal behavioral RED; this corrected source still awaits actual qualification.
