; Native written Nu syntax; command execution and dynamic lookup are separate.
(nu_script) @root @module @scope
(ERROR) @expression
(decl_def) @declaration @scope @signature
(decl_def unquoted_name: (cmd_identifier) @definition)
(decl_def quoted_name: (val_string (string_content) @definition))
(decl_module body: (block)) @declaration @scope @signature
(decl_module unquoted_name: (cmd_identifier) @definition body: (block))
(decl_module quoted_name: (val_string (string_content) @definition) body: (block))
(parameter param_name: (identifier)) @declaration @signature
(parameter param_name: (identifier) @definition)
[(stmt_let) (stmt_mut) (stmt_const)] @declaration @signature
(stmt_let name: (identifier) @definition)
(stmt_mut name: (identifier) @definition)
(stmt_const name: (identifier) @definition)
(val_variable name: (identifier) @reference)
(param_completer command: (cmd_identifier) @reference)
(param_completer command: (val_string (string_content) @reference))
[(decl_use) (decl_extern) (decl_alias) (command)] @expression
(comment) @comment
(val_string) @string
