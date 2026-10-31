; Native graph containers retain written labels without claiming RDF declarations.
(document) @root @module
(graph) @module
(graph [(iri_reference) (prefixed_name) (blank_node_label) (anon)] @signature)
[(prefix_id) (sparql_prefix)] @declaration @signature
[(prefix_id (namespace (pn_prefix) @definition))
 (sparql_prefix (namespace (pn_prefix) @definition))]
(triple (subject) @definition) @declaration
(graph (subject) @declaration @definition)
