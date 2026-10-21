; Native written headers require a complete clean physical source proof.
[(module) (ERROR)] @root @module @scope
(binders) @signature
(def) @declaration @definition @signature
(explicit_binder) @declaration @definition @signature
; Ordinary names remain observable when a required own-formal proof declines.
(identifier) @reference
(identifier) @reference @signature
[(line_comment) (block_comment) (doc_comment) (module_doc_comment)] @comment
[(str_lit) (raw_string)] @string
