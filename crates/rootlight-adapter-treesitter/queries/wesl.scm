; WESL has its own native names and written import syntax.
; Only named functions and their direct formal parameters are declarations.
(translation_unit) @root @module @scope
(function_decl) @declaration @scope @signature
(function_header name: (identifier) @definition)
(param) @declaration
(param name: (identifier) @definition)
[(param_list) (compound_statement)] @scope
(identifier) @reference
[(import_statement) (preproc_bevy_import)] @reference
(global_decl) @expression
[(line_comment) (block_comment)] @comment
