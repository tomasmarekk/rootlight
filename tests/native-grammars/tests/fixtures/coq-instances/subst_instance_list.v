Instance subst_instance_list A `{UnivSubst A} : UnivSubst (list A) :=
  fun u => List.map (subst_instance u).