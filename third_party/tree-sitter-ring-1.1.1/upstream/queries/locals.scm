; Ring language - Local scope definitions
; Used for scope-aware highlighting and navigation

; ====================
; Scope Definitions
; ====================

(function_definition) @local.scope
(class_definition) @local.scope
(package_definition) @local.scope
(if_statement) @local.scope
(while_statement) @local.scope
(for_statement) @local.scope
(switch_statement) @local.scope
(try_statement) @local.scope
(do_again_statement) @local.scope
(anonymous_function) @local.scope
(brace_expression) @local.scope

; ====================
; Definitions
; ====================

; Function parameters
(param_list
  (identifier) @local.definition.parameter)

; Function names
(function_definition
  name: (identifier) @local.definition.function)

; Class names
(class_definition
  name: (identifier) @local.definition.type)

; For loop variable
(for_statement
  variable: (identifier) @local.definition.var)

; ====================
; References
; ====================

(identifier) @local.reference
