// Parse-only atomic-type control; names need not resolve.
// The following declaration is an independent ownership boundary.
module TypeExample
type Flags =
    | First =
        (1u (* shift operand *)
         <<< 0)
    | Next = (2u)
val following: int
