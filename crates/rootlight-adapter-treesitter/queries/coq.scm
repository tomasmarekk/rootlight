; Coq names are authored declarations; terms do not prove elaboration or calls.
[(source_file) (ERROR)] @root @module
(source_file) @signature
(ident_decl) @declaration @definition @signature
[(inductive_definition) (constructor) (module_command) (module_type_command)] @declaration @definition @signature
[(reference) (ident) (dotted_qualid)] @reference
(comment) @comment
(string) @string

; Recovery, custom syntax and unmodeled sentences retain physical gap witnesses.
[(ERROR) (generic_tactic_body) (custom_notation_block) (sentence) (proof_block)] @signature
[(binder) (module_binder)] @signature
[(instance_command) (declare_instance_command) (existing_instance_command) (existing_class_command)] @signature

[(record_command) (class_command)] @signature
[(record_field) (class_singleton_field) (record_constructor)] @declaration @definition @signature
