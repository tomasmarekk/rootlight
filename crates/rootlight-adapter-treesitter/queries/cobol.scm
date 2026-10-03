; COBOL captures describe written programs, storage declarations and entry headers.
; Format selection belongs to the immutable native language, not these selectors.
(start) @root @scope
(program_definition) @declaration @definition @signature
(program_definition) @scope
(data_description) @declaration @definition
(file_description_entry) @declaration @definition
[(section_header) (paragraph_header)] @declaration @definition @signature
(copy_statement) @import
(qualified_word) @reference
(procedure_param) @reference
(call_statement) @reference
(perform_procedure (label) @reference)
(end_program (program_name) @reference)
(comment) @comment
[(string) (x_string) (n_string) (h_string)] @string
