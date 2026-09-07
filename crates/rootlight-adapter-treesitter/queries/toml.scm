; The whole key is one definition; nested dotted-key nodes are not duplicates.
(document) @root @module @scope
(pair . [(bare_key) (quoted_key) (dotted_key)] @definition) @declaration
(table . [(bare_key) (quoted_key) (dotted_key)] @definition) @declaration
(table_array_element . [(bare_key) (quoted_key) (dotted_key)] @definition) @declaration

; Scalars must count toward array positions before nested table ownership resolves.
[(array) (inline_table)] @scope
(array [
  (string) (integer) (float) (boolean)
  (offset_date_time) (local_date_time) (local_date) (local_time)
  (array) (inline_table)
] @scope)
(string) @string
(comment) @comment @documentation
