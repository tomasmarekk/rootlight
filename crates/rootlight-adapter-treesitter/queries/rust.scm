; Reviewed Tier C/D structural evidence for the pinned Rust grammar.
; Captures are a closed Rootlight contract, not public Tree-sitter identities.

(source_file) @root

(mod_item
  name: (identifier) @definition) @module

[
  (function_item)
  (struct_item)
  (enum_item)
  (trait_item)
  (type_item)
  (const_item)
  (static_item)
] @declaration

[
  (function_item name: (identifier) @definition)
  (struct_item name: (type_identifier) @definition)
  (enum_item name: (type_identifier) @definition)
  (trait_item name: (type_identifier) @definition)
  (type_item name: (type_identifier) @definition)
  (const_item name: (identifier) @definition)
  (static_item name: (identifier) @definition)
]

(use_declaration) @import
(parameters) @signature
(block) @scope
(identifier) @reference
(type_identifier) @reference
[(line_comment) (block_comment)] @comment

((line_comment) @documentation
  (#match? @documentation "^//[/!]"))

((block_comment) @documentation
  (#match? @documentation "^/\\*[*!]"))

(string_literal) @string
