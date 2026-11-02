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
