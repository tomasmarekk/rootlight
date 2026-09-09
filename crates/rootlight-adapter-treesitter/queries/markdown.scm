; Written block ownership and exact heading/reference-label source ranges.
; Inline and embedded bodies stay explicit so later interpretation cannot be
; mistaken for completed block-only analysis.
(document) @root @module
(atx_heading) @declaration
(atx_heading (inline) @definition)
(setext_heading (paragraph (inline) @definition)) @declaration
(link_reference_definition (link_label) @definition) @declaration
(paragraph) @string
[(inline) (atx_heading) (setext_heading)
 (fenced_code_block) (indented_code_block) (html_block)
 (minus_metadata) (plus_metadata)] @signature
