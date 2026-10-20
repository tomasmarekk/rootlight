# Local binding

The complete upstream tree is retained under `upstream/`. The selected generated
parser and headers are byte-identical copies from that tree. The local Rust
binding pins existing reviewed build dependencies and compiles these static C11
inputs without source generation, package scripts or network access. There is no
external scanner. Native parse results do not qualify complete Ada semantics.
