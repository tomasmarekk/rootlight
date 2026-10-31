; Default prefix declarations retain binding evidence without an invented name.
(document) @root @module
[(prefix_id) (sparql_prefix)] @declaration @signature
[(prefix_id (namespace (pn_prefix) @definition))
 (sparql_prefix (namespace (pn_prefix) @definition))]

(triple (subject) @definition) @declaration
