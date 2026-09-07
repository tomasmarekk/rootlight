; CSS declarations retain raw selectors and escaped identifiers as source evidence.
; Value functions are not programming-language calls; cascade resolution is separate.
(stylesheet) @root @module @scope
(rule_set (selectors) @definition) @declaration @scope
(keyframes_statement (keyframes_name) @definition) @declaration @scope
((declaration (property_name) @definition) @declaration
 (#match? @definition "^--"))
(import_statement) @import
(block) @scope
(keyframe_block_list) @scope
(comment) @comment @documentation
(js_comment) @comment
(string_value) @string
