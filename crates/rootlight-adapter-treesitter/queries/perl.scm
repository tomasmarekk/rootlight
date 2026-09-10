; Perl written declarations retain native names and source extents.
; Package ownership and reference resolution require a separate binding pass.
(source_file) @root @module @scope
(package_statement name: (package) @definition) @declaration
(class_statement name: (package) @definition) @declaration
(role_statement name: (package) @definition) @declaration
(subroutine_declaration_statement name: (bareword) @definition) @declaration @scope @signature
(method_declaration_statement name: (bareword) @definition) @declaration @scope @signature
(block) @scope
(mandatory_parameter . (scalar (varname)) @definition) @declaration
(optional_parameter . (scalar (varname)) @definition) @declaration
(named_parameter . (scalar (varname)) @definition) @declaration
(slurpy_parameter . [(array (varname)) (hash (varname))] @definition) @declaration
; Variable-list groups have their own nodes; initializer reads are not bindings.
(variable_declaration variable: [(scalar (varname)) (array (varname)) (hash (varname))] @declaration @definition)
(variable_declaration variables: [(scalar (varname)) (array (varname)) (hash (varname))] @declaration @definition)
(variable_group variables: [(scalar (varname)) (array (varname)) (hash (varname))] @declaration @definition)
(refalias_variable [(scalar (varname)) (array (varname)) (hash (varname))] @declaration @definition)
[(scalar) (array) (hash) (function)] @reference
(use_statement module: (package) @reference)
(method_call_expression) @reference
[(package_statement name: (package) @reference)
 (class_statement name: (package) @reference)
 (role_statement name: (package) @reference)]
(comment) @comment
(pod) @documentation
[(string_literal) (interpolated_string_literal) (command_string)] @string
