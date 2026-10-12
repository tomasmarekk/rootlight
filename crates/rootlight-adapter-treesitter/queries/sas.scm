; SAS identities come from direct native owners and written name fields.
; Opaque statements and runtime expansion remain separate coverage evidence.
(program) @root @module @scope
[(fcmp_function_definition) (fcmp_subroutine_definition) (macro_definition)] @declaration @definition @signature
[(fcmp_function_definition) (fcmp_subroutine_definition) (macro_definition)
 (data_step) (proc_step) (proc_sql_step) (macro_if_statement) (macro_do_statement)] @scope
[(fcmp_parameter) (macro_parameter)] @declaration @definition
(dataset_name) @declaration @definition @signature
[(fcmp_function_definition) (fcmp_subroutine_definition) (macro_definition)
 (data_step) (proc_step) (proc_sql_step) (macro_if_statement) (macro_do_statement)
 (generic_statement) (fcmp_unknown_statement) (include_statement)] @expression
(dataset_name) @expression
[(macro_call) (macro_call_statement) (macro_variable_ref) (dataset_name)] @reference
[(block_comment) (percent_comment) (line_comment)] @comment
(string_literal) @string
