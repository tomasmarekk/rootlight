; RON source data properties and literals never imply Rust bindings or calls.
; Constructor and enum spelling stays data rather than callable declarations.
(source_file) @root @module @scope
[(map) (struct) (tuple) (array)] @scope
; Every value, including scalars and units, consumes one positional address.
(array [(map) (struct) (tuple) (array) (string) (char) (boolean) (integer) (float) (negative) (enum_variant)] @scope)
(tuple [(map) (struct) (tuple) (array) (string) (char) (boolean) (integer) (float) (negative) (enum_variant)] @scope)
(source_file [(map) (struct) (tuple) (array) (string) (char) (boolean) (integer) (float) (negative) (enum_variant)] @scope)
(struct_entry . (identifier) @definition) @declaration
(map_entry . (string) @definition) @declaration
(string) @string
[(line_comment) (block_comment)] @comment @documentation
