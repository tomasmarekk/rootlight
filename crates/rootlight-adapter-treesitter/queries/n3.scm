; Native Notation3 has its own grammar, including formulas and quantifiers.
; Each quantified term owns its definition; formulas supply lexical identity only.
(n3_doc) @root @module
[(prefix_id) (sparql_prefix)] @declaration
[(prefix_id (pname_ns (pn_prefix) @definition))
 (sparql_prefix (pname_ns (pn_prefix) @definition))]
[(universal [(iri_reference) (prefixed_name)] @declaration @definition)
 (existential [(iri_reference) (prefixed_name)] @declaration @definition)]
(formula) @scope
