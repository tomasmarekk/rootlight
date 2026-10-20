; Names and ownership require the Clojure form/context proof, not list spelling.
(source) @root @module @scope
(list_lit) @declaration @definition @signature
(list_lit) @scope
; Optional expressions must survive the mandatory identity-pattern partition.
(list_lit) @expression
(sym_lit) @reference
[(quoting_lit) (syn_quoting_lit) (read_cond_lit) (splicing_read_cond_lit)
 (evaling_lit) (tagged_or_ctor_lit) (ERROR)] @expression
(comment) @comment
(str_lit) @string
