# Local VB.NET binding and final physical declaration

The authenticated upstream revision is 538b7087bf80e86004531b392fe1186379c0a2b5.
The original 17-file archive and complete authored payload are retained. The
explicit upstream LICENSE is MIT; upstream package.json separately says ISC.
That package metadata is preserved as inert source rather than executed or used
as the local crate's manifest. This local Rust package is unpublished.

grammar.js contains the reviewed source-file final-declaration alternative and
the corresponding factored conflict participants. A final namespace/class/
structure/interface/module/enum can end at physical EOF without an appended
newline. Ordinary interior declarations keep their terminator rule. The literal
overlay is patches/grammar.patch. Generated parser files are copied unchanged
from two byte-equal ABI14 productions; parser.c is not hand edited.

The local build compiles only src/parser.c with the retained generated headers.
It runs no generator, downloader, upstream build.py, npm script or prebuilt
object. build.py and parser.obj/parser.lib/parser.exp are preserved with .inert
suffixes; .gitignore is retained as .gitignore.upstream. The archive and source
inventory retain their original names, hashes and mappings.

Finite native qualification preserved the complete BOM/CRLF/no-final-newline
source and all 42 generic expected outcomes. All 17 upstream corpus cases still
have the same baseline/preferred failures; no new regression is a narrower
claim than a green upstream corpus. Native grammar qualification alone does not
qualify product lowering, whole-language coverage or an installed package.
