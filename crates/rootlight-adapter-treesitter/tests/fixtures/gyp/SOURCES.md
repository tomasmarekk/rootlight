# Native GYP source fixtures

These fixtures preserve the exact physical corpus bytes used for native GYP
behavior checks. They are upstream test inputs, not Rootlight-authored examples.
The existing native Python grammar supplies only the literal AST; no Python
execution or build configuration evaluation is performed.

| Fixture | Pinned source | Git blob | Physical bytes / SHA-256 | Git-byte equality |
| --- | --- | --- | --- | --- |
| `binding-deno.gyp` | `10_Rust_deno` at `ce9bc924abea6a0d0839b3a53438e4da44d08cd7`, `tests/registry/npm/@denotest/node-addon/1.0.0/binding.gyp` | `5f32930641224dbb002c55c6772347fd8a029302` | 107 / `0b3b12a3a68dad29785a223e8952836a3b414d90d197bbec704e5c0083f0f00c` | exact |
| `uv-dlopen.gyp` | `193_GYP_electron` at `9c80dad42fb42a445c92e83419b458bb3a785f6c`, `spec/fixtures/native-addon/uv-dlopen/binding.gyp` | `6d8d0d39b0dded843c5c5974c20e84cf0ff55068` | 210 / `fc782ab36c52809e06d63be0e7c440f818c02f2047307613d57723738cf8846c` | exact |
| `mouse-input.gyp` | `193_GYP_electron` at `9c80dad42fb42a445c92e83419b458bb3a785f6c`, `spec/fixtures/native-addon/mouse-input/binding.gyp` | `7531ae79d9c75856bb59632b2df892722e66afbc` | 1598 / `55b0b9793417a6195f3393a2770e2b1a90cc5e114196c4f911dba4928c651527` | exact |
| `virtual-display.gyp` | `193_GYP_electron` at `9c80dad42fb42a445c92e83419b458bb3a785f6c`, `spec/fixtures/native-addon/virtual-display/binding.gyp` | `aff671e3175bb9a1a9901d5de1167bb6c7e74531` | 872 / `96b7f310c419d96092c56021bc7ccae6e519e7e609eb106eaaa0c8ea52bbc527` | exact |
| `dialog-helper.gyp` | `193_GYP_electron` at `9c80dad42fb42a445c92e83419b458bb3a785f6c`, `spec/fixtures/native-addon/dialog-helper/binding.gyp` | `6c7f821b585da4ed2351e08b0b2ca7a528a9f65a` | 819 / `195397936b64b0e7d59af48b8517d3a44f71e8f4f0508cc94bc3786a471745bd` | exact |
| `echo.gyp` | `193_GYP_electron` at `9c80dad42fb42a445c92e83419b458bb3a785f6c`, `spec/fixtures/native-addon/echo/binding.gyp` | `642567ad5229752858c08788abf23448539b2960` | 112 / `ab9126e23b429ea16d88ffd7e4e811d804e2a865d021a1a1210009a6b63c0970` | exact |
| `is-valid-window.gyp` | `193_GYP_electron` at `9c80dad42fb42a445c92e83419b458bb3a785f6c`, `spec/fixtures/native-addon/is-valid-window/binding.gyp` | `0de7c1f9d17190e16fa880076d4a54d8eb1486f8` | 886 / `c28bd2f9b2d0f5678201805d3bf1493f34be10d5ad77b205914f8fd42dec1c9b` | exact |
| `object-wrap.gyp` | `193_GYP_electron` at `9c80dad42fb42a445c92e83419b458bb3a785f6c`, `spec/fixtures/native-addon/object-wrap/binding.gyp` | `e0dc6909957faf17d7493e73681699ecb6f32a30` | 119 / `ac8582ec59d2c6ea76a2aadc46937e4dd860b9ec8b2daf3bee6f409fa3b073e6` | exact |

The original Deno implicit-node-gyp fixture has the same physical bytes as
`binding-deno.gyp`. Its separately authored driver is retained by the corpus;
the tests do not infer node-gyp invocation from the directory name.

Implicit input: `tests/registry/npm/@denotest/node-addon-implicit-node-gyp/1.0.0/binding.gyp`, Git blob `5f32930641224dbb002c55c6772347fd8a029302`, 107 physical bytes,
SHA-256 `0b3b12a3a68dad29785a223e8952836a3b414d90d197bbec704e5c0083f0f00c`.

| Retained license | Pinned upstream path | Git blob | Physical bytes / SHA-256 |
| --- | --- | --- | --- |
| `LICENSE.DENO` | `ce9bc924abea6a0d0839b3a53438e4da44d08cd7:LICENSE.md` | `b641c75c0b04ccfddcb1145d2554ab49b6ca9411` | 1074 / `f62497fffecc0852960c8d3e6934b9db86d16396e9b604072e923892cae3a588` |
| `LICENSE.ELECTRON` | `9c80dad42fb42a445c92e83419b458bb3a785f6c:LICENSE` | `536d54efc3fd36ff7e079f82ce55f82c887758cd` | 1096 / `5154e165bd6c2cc0cfbcd8916498c7abab0497923bafcd5cb07673fe8480087d` |
| `LICENSE.CHROMIUM` | `9c80dad42fb42a445c92e83419b458bb3a785f6c:chromium_src/LICENSE.chromium` | `6f2d1444dd98377af3e589ad47afb3fc45ee597b` | 1558 / `9873d81def9cebb9598b4a2d0a1ad062b17781c35c25cc04b75a0f62fc8667fd` |
