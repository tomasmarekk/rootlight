; Source markup ownership, not browser DOM construction or identifier bindings.
; Embedded bodies are required context so bounded extraction cannot hide gaps.
(document) @root @module
(element (start_tag (tag_name) @definition)) @declaration
(element (self_closing_tag (tag_name) @definition)) @declaration
(script_element (start_tag (tag_name) @definition)) @declaration
(style_element (start_tag (tag_name) @definition)) @declaration
(attribute (attribute_name) @definition) @declaration
(raw_text) @signature
(erroneous_end_tag) @signature
[(attribute_value) (text) (entity)] @string
(comment) @comment @documentation
