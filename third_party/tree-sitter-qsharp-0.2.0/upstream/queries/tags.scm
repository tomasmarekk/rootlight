; tags.scm — Symbol outline / breadcrumbs / code navigation for Q#

(callable_decl
  name: (identifier) @name) @definition.function

(struct_decl
  name: (identifier) @name) @definition.type

(newtype_decl
  name: (identifier) @name) @definition.type

(namespace
  name: (path_or_identifier) @name) @definition.module

(typed_param
  name: (identifier) @name) @definition.parameter

(let_statement
  (pattern (identifier) @name)) @definition.variable

(mutable_statement
  (pattern (identifier) @name)) @definition.variable

(for_expression
  (pattern (identifier) @name)) @definition.variable

(field_def
  name: (identifier) @name) @definition.field

(call_expression
  (identifier) @name) @reference.call

(call_expression
  (path
    (identifier) @name .)) @reference.call
