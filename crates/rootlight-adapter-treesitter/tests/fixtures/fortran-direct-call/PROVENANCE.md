# Fortran direct-call fixture

The original `src/array/stdlib_array.f90` from the Fortran stdlib project is retained unchanged under its MIT license. Its complete physical source is 1,981 bytes with SHA-256 `d3a2ca64647a6a71850fa7b7f328bfac6fcf92c35b27e75f1d36c3a547065182`. See the adjacent original license.

Upstream project: https://github.com/fortran-lang/stdlib

Retained checkout revision: `9a15c7772f1a76a6c497b9f3abb793841fc81f74`. Source: https://github.com/fortran-lang/stdlib/blob/9a15c7772f1a76a6c497b9f3abb793841fc81f74/src/array/stdlib_array.f90

The digest identifies the physical fixture bytes, including their original line endings.

The regression preserves authored module/function/subroutine names and byte spans. It tests same-module direct `CALL` binding without claiming template expansion, imported procedures, compiler legality or complete Fortran semantics.
