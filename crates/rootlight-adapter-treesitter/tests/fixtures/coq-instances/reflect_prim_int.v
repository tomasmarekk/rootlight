#[program,global] Instance reflect_prim_int : ReflectEq Numbers.Cyclic.Int63.Uint63.int :=
  { eqb := Numbers.Cyclic.Int63.Uint63.eqb }.