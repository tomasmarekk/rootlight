; Authored Groovy declarations with exact names and native lexical owners.
; Runtime metaprogramming and receiver dispatch are not inferred from spelling.

(source_file) @root @module
(package_declaration name: (qualified_name) @definition) @scope @module

[
  (class_declaration name: (identifier) @definition)
  (interface_declaration name: (identifier) @definition)
  (annotation_type_declaration name: (identifier) @definition)
  (trait_declaration name: (identifier) @definition)
  (record_declaration name: (identifier) @definition)
  (enum_declaration name: (identifier) @definition)
  (enum_constant name: (identifier) @definition)
  (method_declaration name: [(identifier) (quoted_identifier)] @definition)
  (constructor_declaration name: (identifier) @definition)
  (variable_declarator name: (identifier) @definition)
  (formal_parameter name: (identifier) @definition)
  (closure_parameter name: (identifier) @definition)
] @declaration

[(method_declaration) (constructor_declaration)] @signature
[(block) (class_body) (enum_body)] @scope
[(closure) (gstring_closure)] @scope @declaration @signature
(import_declaration) @import
(method_invocation) @call
[(identifier) (type_identifier)] @reference
[(line_comment) (block_comment)] @comment
((block_comment) @documentation (#match? @documentation "^/\\*\\*"))
(string_literal) @string
