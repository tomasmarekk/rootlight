# Native Vento source fixture

`badly_formatted.vto` preserves the exact physical and Git bytes of
`tests/specs/fmt/vento/badly_formatted.vto` from `https://github.com/denoland/deno`.
It is an upstream formatting input, not Rootlight-authored source or rendered output.

| Corpus representative | Pinned commit | Git blob |
| --- | --- | --- |
| `10_Rust_deno` | `ce9bc924abea6a0d0839b3a53438e4da44d08cd7` | `ad46443904d84067339bee27ec219c21893e2ef6` |
| `192_Vento_deno` | `e7b39ebd55c85aaf6a29290c1f5a99e2e21e09dd` | `ad46443904d84067339bee27ec219c21893e2ef6` |

The fixture is 52 bytes, SHA-256
`1c6650e55aa4f9a3e15a70d57cdc390e6f183a9e9b726de8f8046ba8cc8b8d2f`.
Both representatives contain the same bytes from the same upstream project;
they do not establish independent project coverage.

`LICENSE.DENO` preserves the original representative's complete `LICENSE.md`,
Git blob `b641c75c0b04ccfddcb1145d2554ab49b6ca9411`, 1074 bytes, SHA-256
`f62497fffecc0852960c8d3e6934b9db86d16396e9b604072e923892cae3a588`.
Native syntax tests do not establish template rendering, JavaScript bindings,
service integration or complete language coverage.
