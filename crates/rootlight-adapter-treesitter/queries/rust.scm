; Reviewed structural evidence for the pinned Rust grammar.
; Captures are a closed Rootlight contract, not public Tree-sitter identities.

(source_file) @root
(source_file) @signature

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
(closure_parameters) @signature
(function_item body: (_) @signature)
(closure_expression body: (_) @signature)
(closure_expression) @declaration
(let_declaration) @signature
(macro_invocation) @signature
[(attribute_item) (inner_attribute_item) (use_declaration) (extern_crate_declaration)
 (macro_definition) (function_item) (mod_item) (impl_item) (trait_item)
 (const_item) (static_item) (struct_item) (enum_item) (union_item) (type_item)
 (foreign_mod_item)] @signature
(or_pattern) @signature
[(for_expression) (match_arm) (if_expression) (while_expression)] @signature
[(for_expression) (match_arm) (if_expression) (while_expression)] @scope
(if_expression condition: (_) @signature)
(while_expression condition: (_) @signature)
[(identifier) (shorthand_field_identifier)] @declaration
[(identifier) (shorthand_field_identifier)] @definition
(impl_item
  type: (_) @scope_type) @scope
(impl_item
  trait: (_) @scope_trait)
(block) @scope
(call_expression
  function: [
    (identifier)
    (field_expression
      field: (field_identifier))
  ] @call)
(call_expression
  function: (scoped_identifier) @scoped_call)
(call_expression
  function: (generic_function) @call)
((attribute_item) @test_attribute
  (#match? @test_attribute "^#\\[\\s*test\\s*\\]$"))
(identifier) @reference
(shorthand_field_identifier) @reference
(type_identifier) @reference
[(line_comment) (block_comment)] @comment

((line_comment) @documentation
  (#match? @documentation "^//[/!]"))

((block_comment) @documentation
  (#match? @documentation "^/\\*[*!]"))

(string_literal) @string
