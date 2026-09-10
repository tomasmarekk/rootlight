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
(comment) @comment
(pod) @documentation
[(string_literal) (interpolated_string_literal) (command_string)] @string
