Global Instance Monad_result {E} : Monad (fun T => result T E) :=
  {| ret _ t := Ok t;
     bind _ _ mt f :=
     match mt with
     | Ok t => f t
     | Err e => Err e
     end |}.