; Native SPARQL requests have their own grammar and complete source boundary.
; Only explicit AS targets are certified as written alias declarations.
(unit) @root @module
(prefix_declaration) @declaration
(prefix_declaration (namespace (pn_prefix) @definition))
[(select_clause bound_variable: (var) @declaration @definition)
 (bind bound_variable: (var) @declaration @definition)]
[(select_query) (sub_select) (construct_query) (describe_query) (ask_query) (modify)] @scope
