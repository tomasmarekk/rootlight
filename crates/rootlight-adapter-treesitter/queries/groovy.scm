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
  (catch_formal_parameter name: (identifier) @definition)
  (resource name: (identifier) @definition)
  (type_parameter name: (type_identifier) @definition)
  (record_component name: (identifier) @definition)
] @declaration

(for_in_statement variable: (identifier) @declaration @definition)

[(method_declaration) (constructor_declaration)] @signature
[(block) (class_body) (enum_body)] @scope
[(for_statement) (for_in_statement) (catch_clause) (try_statement)] @scope
[(closure) (gstring_closure)] @scope @declaration @signature
(import_declaration) @import
(method_invocation) @call
[(identifier) (type_identifier)] @reference
[(line_comment) (block_comment)] @comment
((block_comment) @documentation (#match? @documentation "^/\\*\\*"))
(string_literal) @string
