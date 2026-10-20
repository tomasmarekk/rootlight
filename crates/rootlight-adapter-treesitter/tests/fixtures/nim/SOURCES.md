# Nim source fixture

`patterns.nim` is the exact 4,453-byte Jester source, SHA-256
`fd01db61fde5353ed1adf126da8672d57622570f2affde9d03f9dab191f68dfd`,
from `dom96/jester` at the corpus snapshot. Its original copyright header and
MIT license are retained. The finite tests assert written object/field/procedure
definitions and one direct lexical call, without claiming exhaustive semantics.

The unchanged `tests/config.nims` and `jester.nimble` from the same MIT-licensed
snapshot exercise native source acceptance separately from task or compiler
evaluation. They are 43 and 482 physical bytes respectively. No package command,
dependency requirement or task body in these inert fixtures is executed.
