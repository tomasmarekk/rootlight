This package retains the complete 35-blob upstream tree at revision
`eed7ddf6a66b596906aa8ca3d40521b8278adc6f` under `upstream/`, with original
MIT authorship and license. Active generated C uses ABI 15. The local grammar
adds ordinary named foreach bindings (`..`, `& ..`, `=>`), and an explicit
generator metadata file prevents the CLI's undocumented ABI-14 fallback when
metadata is absent. Generated tables and the CLI's updated array header are
retained. The authored Rust wrapper uses the workspace's LanguageFn interface;
the preserved upstream wrapper's older runtime is inactive.

Compilation invokes only the declared C compiler on local parser tables. It
does not run a generator, download sources, or execute upstream build scripts.
The private native probe demonstrates grammar feasibility, not Rootlight
provider behavior, complete Hare semantics, or installed-package qualification.
