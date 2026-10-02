; Haskell captures describe written declarations and applications, not inferred types.
; Native selectors separate shared expression, pattern and type node kinds.
(haskell) @root @module @scope
(header) @declaration @definition @signature
(import) @import
; The pinned runtime stores at most three captures on one query step.
; Separate lexical scopes so they cannot silently disappear as a fourth capture.
[(function) (bind)] @declaration @definition @signature
[(data_type) (newtype) (type_synomym) (class)] @declaration @definition @signature
[(data_constructor) (newtype_constructor) (gadt_constructor)] @declaration @definition @signature
[(function) (bind) (data_type) (newtype) (type_synomym) (class) (data_constructor) (newtype_constructor) (gadt_constructor)] @scope
(signature) @signature
(field) @signature
[(variable) (operator) (constructor_operator) (constructor) (name)] @declaration @definition
[(variable) (operator) (constructor_operator) (constructor) (name) (qualified) (prefix_id) (infix_id) (field_name) (module)] @reference
[(instance) (local_binds) (let_in) (let) (lambda) (lambda_case) (lambda_cases) (alternative) (do) (rec) (list_comprehension) (generator)] @scope
(instance) @signature
(lambda) @signature
(expression/apply) @call @call_name
(expression/infix) @call @call_name
(comment) @comment
(haddock) @comment @documentation
(string) @string
