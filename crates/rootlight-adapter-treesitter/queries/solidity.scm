; Solidity source identities retain declaration ownership without executing contracts.
; Qualified calls and embedded assembly remain distinguishable from lexical names.
(source_file) @root @module
(contract_declaration name: (identifier) @definition) @declaration @scope
(interface_declaration name: (identifier) @definition) @declaration @scope
(library_declaration name: (identifier) @definition) @declaration @scope
(struct_declaration name: (identifier) @definition) @declaration @scope
(enum_declaration name: (identifier) @definition) @declaration @scope
(enum_value) @definition @declaration
(user_defined_type_definition name: (identifier) @definition) @declaration
(function_definition name: (identifier) @definition) @declaration @scope @signature
(constructor_definition "constructor" @definition) @declaration @scope @signature
(fallback_receive_definition ["fallback" "receive"] @definition) @declaration @scope @signature
(modifier_definition name: (identifier) @definition) @declaration @scope @signature
(event_definition name: (identifier) @definition) @declaration
(error_declaration name: (identifier) @definition) @declaration
(state_variable_declaration name: (identifier) @definition) @declaration
(constant_variable_declaration name: (identifier) @definition) @declaration
(struct_member name: (identifier) @definition) @declaration
(variable_declaration name: (identifier) @definition) @declaration
(parameter name: (identifier) @definition) @declaration
(event_parameter name: (identifier) @definition) @declaration
(error_parameter name: (identifier) @definition) @declaration
(import_directive) @import
(function_body) @scope
(block_statement) @scope
(assembly_statement) @scope
(call_expression) @call
(call_expression function: (expression [(identifier) (member_expression)] @call_name))
(identifier) @reference
(user_defined_type) @reference
(comment) @comment
((comment) @documentation (#match? @documentation "^(///|/\\*\\*)"))
(string) @string
