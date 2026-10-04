// Parse-only atomic-type control; names need not resolve.
// The following declaration is an independent ownership boundary.
module TypeExample
type Flags =
    | First = (1u)
    | Next = (1u <<< 0)
val following: int
