; Physical Fortran headers and explicit CALL sites retain native ownership.
; Template and binding gaps are source evidence, not expanded declarations.
; The pinned runtime retains at most three captures on one query step.
; Optional gaps also need separate patterns from the mandatory identity scan.
(translation_unit) @root @module @scope
[(module) (submodule)] @module @definition @signature
[(program) (function) (subroutine) (module_procedure) (derived_type_definition) (interface)] @declaration @definition @signature
[(module) (submodule) (program) (function) (subroutine) (module_procedure) (derived_type_definition) (interface)] @scope
[(translation_unit) (module) (submodule) (program) (function) (subroutine) (module_procedure) (derived_type_definition) (interface)] @expression
(variable_declaration declarator: (_) @declaration @definition)
(variable_declaration declarator: (_) @expression)
(parameters (identifier) @declaration @definition)
(parameters (identifier) @expression)
(subroutine_call) @call @call_name
(use_statement) @import
(identifier) @reference
(call_expression) @reference
(comment) @comment
(string_literal) @string
