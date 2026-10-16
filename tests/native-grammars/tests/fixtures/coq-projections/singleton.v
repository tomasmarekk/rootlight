Class t := preserves_obs : forall p p' (pr : o.(pre) p) (pr' : o.(pre) p'),
        extends p p' -> extends' (o.(transform) p pr) (o.(transform) p' pr').