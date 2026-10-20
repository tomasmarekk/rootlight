; Protocol Buffers captures retain complete physical owners and written type paths.
; Option, extension and recovered syntax remains explicit source evidence.

(source_file) @root @module @scope
(source_file) @expression

[(message) (enum) (enum_field) (field) (map_field) (oneof)
 (oneof_field) (service) (rpc) (package)] @declaration @definition @signature

(message_or_enum_type) @reference
(import path: (string) @import)
(comment) @comment
(string) @string
[(syntax) (import) (option) (field_option) (enum_value_option) (reserved)
 (extensions) (extend) (oneof_field) (ERROR)] @expression
