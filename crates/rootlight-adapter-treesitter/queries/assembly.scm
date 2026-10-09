; Physical owners retain exact source boundaries, not assembled bindings.
; Instruction operands and directives remain unproved source contexts.
(program) @root @module @scope
[(procedure_definition) (macro_definition) (segment_definition)
 (data_definition) (constant_definition) (label) (const)] @declaration @definition @signature
[(procedure_definition) (macro_definition) (segment_definition)] @scope
[(program) (procedure_definition) (macro_definition) (segment_definition)
 (data_definition) (constant_definition) (label) (const)
 (instruction) (meta) (ERROR)] @expression
[(line_comment) (block_comment)] @comment
[(string) (masm_string)] @string
