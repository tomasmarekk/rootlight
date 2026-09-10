; Nix authored bindings and source expressions, never evaluated package attributes.
; Native selectors preserve quoted paths and separate parameters from ordinary reads.
(source_code) @root @module
(binding) @declaration @definition @signature
(binding attrpath: (attrpath attr: (_) @definition_part))
(function_expression) @scope @declaration @signature
(function_expression universal: (identifier) @declaration @definition)
(formal) @declaration @definition
(inherited_attrs attr: (_) @declaration @definition)
(inherit attrs: (inherited_attrs attr: (_) @reference))
[(let_expression) (attrset_expression) (rec_attrset_expression) (let_attrset_expression) (with_expression)] @scope
(apply_expression) @call
(apply_expression function: (variable_expression) @call_name)
(variable_expression) @reference
(select_expression) @reference
(comment) @comment
((comment) @documentation (#match? @documentation "^/\\*\\*"))
[(string_expression) (indented_string_expression) (path_expression) (hpath_expression) (spath_expression) (uri_expression)] @string
