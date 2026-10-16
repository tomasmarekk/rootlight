Class checker_flags := {
  (* check_guard : bool ; *)

  (* Checking universe constraints iff [true] *)
  check_univs : bool ;

  (* Prop <= Type iff [true] *)
  prop_sub_type : bool ;

  (* If sort of indices are taken in account for the sort of inductive types *)
  indices_matter : bool ;

  (* Lets in constructor types are allowed iff [true] *)
  lets_in_constructor_types : bool
}.