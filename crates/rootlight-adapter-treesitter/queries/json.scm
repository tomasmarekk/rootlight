; Properties retain every occurrence, including duplicate and escaped keys.
; Every array value participates in position accounting, not just containers.
(document) @root @module @scope
[(object) (array)] @scope
(array [(object) (array) (string) (number) (true) (false) (null)] @scope)
(document [(object) (array) (string) (number) (true) (false) (null)] @scope)
(pair key: (string) @definition) @declaration
(string) @string
(comment) @comment @documentation
