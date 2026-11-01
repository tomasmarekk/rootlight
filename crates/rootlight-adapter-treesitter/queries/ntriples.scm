; Native N-Triples statements and direct term fields feed the typed RDF codec.
; Expression captures preserve exact spans without inventing declarations.
(document) @root @module
(triple) @expression
(triple subject: (_) @expression)
(triple predicate: (iri_reference) @expression)
(triple object: (_) @expression)
(comment) @comment
(string) @string
