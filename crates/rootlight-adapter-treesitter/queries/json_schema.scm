; Native schema context selects semantic owners; other members retain addresses.
; Required member/key captures keep pointer proof independent from symbol names.
(document) @root @module @scope
(document) @expression
[(object) (array)] @scope
(array [(object) (array) (string) (number) (true) (false) (null)] @scope)
(pair) @scope @declaration
(pair) @expression
(pair key: (string) @definition @signature)
(pair value: (string) @reference)
(string) @string
