# Raffia source inputs

The complete published `raffia 0.13.0` and `raffia_macro 0.2.0` source archives are retained, including manifests, inert VCS metadata and published examples. They originate from [g-plane/raffia](https://github.com/g-plane/raffia). Published MIT declarations omit the license text; the exact repository licenses at each crate's VCS commit are therefore added separately as `LICENSE` in each local crate. No original published file is omitted.

| Package | Crates.io archive SHA-256 | VCS revision |
| --- | --- | --- |
| raffia 0.13.0 | `0a83eb5987650fcfc3f2e912d3a718f19b8e87c0d980e12e36edf79fa0221d93` | `f5cfbbdff33e4c1370940a48a76d26e5202b151d` |
| raffia_macro 0.2.0 | `8ac21f5e4bc6b29ea5d5e691a9aa08b2b9fd8891f48a99157b9f02d8a48e1318` | `c6cf6c661324f82422d7976ca035f9b5634f768e` |

The reviewed local parser changes retain genuine indentation syntax and exact native spans. Separate typed AST statements represent written `+` includes and `=` mixins. Indentation transitions wait across comments/blank lines for the next substantive token. Local guards reject invalid indentation and expose synchronous parser/scanner checkpoints to the caller's bounded control hook. A typed control stop remains distinct from native syntax rejection. The source manifest identifies every upstream file, local change and added license.

The local parser manifest points its existing macro dependency at the adjacent complete macro sources. Other dependencies remain ordinary exact project-lock inputs from the versioned Cargo source directory; compilation and packaging never acquire source code. Optional upstream features are disabled for the initial provider. The runtime paths and proc macro have exact admissions in `policy/supply-chain.toml`; the project lock and source-closure inventory bind their local inputs. This input bundle alone does not establish an installed, network-isolated build.
