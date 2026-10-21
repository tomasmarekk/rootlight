; Native owners certify literal aliases and COPY parameter boundaries.
; Every FROM, including unnamed stages, retains its stage-boundary witness.
(source_file) @root
(from_instruction) @declaration
(from_instruction) @signature
(from_instruction as: (image_alias) @definition)
(copy_instruction) @signature
(copy_instruction) @reference
(ERROR) @signature
(comment) @comment

; Literal ENV key fields retain their own pair and instruction witnesses.
(env_instruction) @signature
(env_pair) @declaration
(env_pair) @signature
(env_pair name: (unquoted_string) @definition)
