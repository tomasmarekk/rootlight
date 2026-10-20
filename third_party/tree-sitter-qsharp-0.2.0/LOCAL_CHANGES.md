# Reviewed local changes

The grammar accepts both canonical `controlled adjoint` and legacy `adjoint controlled` specialization order. `LOCAL_CHANGES.patch` retains the exact source change. The corresponding generated C and grammar JSON are checked in; original generator inputs and outputs remain exact under `upstream/`. NODE_TYPES and scanner are unchanged. The separate operational wrapper uses exact reviewed local Rust dependencies, compiles C11, and performs no acquisition or generation.
