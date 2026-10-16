    Notation recomp_branch' := (fun x : context * term => {| bbody := x.2; bcontext := x.1 |}).

    Lemma list_map_swap_eq :
      forall l l',
        map decomp_branch l = map decomp_branch l' ->
        l = l'.