; Authored Astro host ownership and complete embedded-region accountability.
; Expressions remain required signatures until their native child analysis succeeds.
(document) @root @module
(element (start_tag (tag_name) @definition)) @declaration
(element (self_closing_tag (tag_name) @definition)) @declaration
(script_element (start_tag (tag_name) @definition)) @declaration
(style_element (start_tag (tag_name) @definition)) @declaration
(attribute (attribute_name) @definition) @declaration
[(frontmatter_js_block) (raw_text) (html_interpolation)
 (attribute_interpolation) (attribute_backtick_string) (erroneous_end_tag)] @signature
[(attribute_value) (text)] @string
(comment) @comment @documentation
