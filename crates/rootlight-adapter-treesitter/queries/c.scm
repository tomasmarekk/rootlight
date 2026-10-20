; Reviewed Tier D structural evidence for the pinned C grammar.
; Preprocessor facts remain syntax only and do not imply evaluated conditions.

(translation_unit) @root
(preproc_include) @module

[
  (function_definition)
  (declaration)
  (struct_specifier)
  (union_specifier)
  (enum_specifier)
  (type_definition)
  (preproc_def)
  (preproc_function_def)
] @declaration

[
  (function_declarator declarator: (identifier) @definition)
  (struct_specifier name: (type_identifier) @definition)
  (union_specifier name: (type_identifier) @definition)
  (enum_specifier name: (type_identifier) @definition)
  (type_definition declarator: (type_identifier) @definition)
  (preproc_def name: (identifier) @definition)
  (preproc_function_def name: (identifier) @definition)
]

(preproc_include) @import
(parameter_list) @signature
[(preproc_def) (preproc_function_def)] @signature
; Written branches remain available while their selection is unevaluated.
[(preproc_if) (preproc_ifdef)] @signature
[(preproc_def) (preproc_function_def)] @string
(compound_statement) @scope
(call_expression) @call
[
  (call_expression
    function: (identifier) @call_name)
  (call_expression
    function: (field_expression
      field: (field_identifier) @call_name))
]
[(identifier) (field_identifier) (type_identifier)] @reference
(comment) @comment

((comment) @documentation
  (#match? @documentation "^/\\*\\*"))

[(string_literal) (concatenated_string)] @string

; Native header ownership rejects nested prototype/function-pointer binders.
(parameter_declaration) @declaration
(parameter_declaration declarator: (_) @definition)

; Keep the native proof reference mandatory without adding identity material.
(function_definition) @signature
(identifier) @reference @signature
