# Native ABAP source fixture

`zif_abapgit_gui_renderable.intf.abap` preserves the exact physical and Git bytes
of `src/ui/core/zif_abapgit_gui_renderable.intf.abap` in
`https://github.com/abapGit/abapGit`, original corpus representative
`39_ABAP_abapGit`, commit `823a9f4c0830ecea4125144c923021e84c68d933`.
Its Git blob is `ac0292a6b46c1b3a549545f1e2049472c9952c94`; it is 186 bytes,
SHA-256 `79a6ab4ceab3ae01b1400df96cd1f9317ea9b6e820dfb510dbf45796d63c3cda`.
This is upstream source data, not a Rootlight-authored interface.

`LICENSE.ABAPGIT` preserves that representative's complete MIT `LICENSE`,
Git blob `6b8c5ff0d5e699be7887b14b851fea48402ae89e`, 1087 bytes, SHA-256
`00006635c99be9c8b21ac770bd1aedf3e171eb6fa9b6199097d092e71babab07`.
It names the abapGit Contributors, 2014.

The native test checks exact interface and method name spans and their AST
containment. It does not establish parameter bindings, semantic indexing,
service integration or coverage of the remaining original ABAP sources.

`zif_abapgit_definitions.intf.abap` preserves the exact physical and Git bytes of
`src/zif_abapgit_definitions.intf.abap` from that same pinned representative.
Its Git blob is `83f3fe336d6b689a592bc21219f157a6c9266de2`; it is 16,094 bytes,
SHA-256 `f56034e1f1d02794462d775025b8b44196a2b7f8b35e595f23228be66d35c24c`.
The same complete `LICENSE.ABAPGIT` notice applies. This second fixture retains
the original source whose generic content hint previously selected C++ when
`.abap` had no default route; it does not establish its unmodeled declarations'
semantic coverage.

The following class witnesses retain exact physical and Git bytes from the same
pinned representative. The complete `LICENSE.ABAPGIT` notice applies to all four.
They test native class owners and direct declaration methods; implementation,
inheritance, visibility, member and dispatch bindings remain separately scoped.

`zcl_abapgit_object_fugs.clas.abap` comes from `src/objects/zcl_abapgit_object_fugs.clas.abap`: 248 bytes,
Git blob `e0d19655e31afd843048d3692b50d7b185068dbb`, SHA-256
`d228d907133f5c99236cecd9a19d791924ee173b9220eb1a39ea8809df40a0b7`.

`zcl_abapgit_object_tabl.clas.testclasses.abap` comes from `src/objects/tabl/zcl_abapgit_object_tabl.clas.testclasses.abap`: 248 bytes,
Git blob `71ff7e28bcda2c34dbd43db691ee71960628aee3`, SHA-256
`67bc1d8ca155b11718043a847dda86a5fd39b6221aeb50c3aed9f9c12edc9a93`.

`zcl_abapgit_diff_factory.clas.abap` comes from `src/diff/zcl_abapgit_diff_factory.clas.abap`: 305 bytes,
Git blob `0dcfd8b6efce8c6aff21e83d03d8a9c8a10e376a`, SHA-256
`70945b405281979b1d05e800cb9564aa66e72abbb9ca5db06cbb93764fcb7250`.

`zcl_abapgit_exit.clas.testclasses.abap` comes from `src/exits/zcl_abapgit_exit.clas.testclasses.abap`: 567 bytes,
Git blob `8344033c74ca082f757712f92ccba856c9a67da3`, SHA-256
`aa10aa06db1013fa9a1758610fa9b31a8b26ca82d66b478ca1cdb6db535659be`.
