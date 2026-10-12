; Pascal identities come from direct written fields and immutable source spans.
; Qualified names, shared declarations and physical calls do not prove bindings.
(root) @root @module @scope
[(unit) (program) (library) (declType) (declProc) (defProc) (declProp)] @declaration @definition @signature
[(unit) (program) (library) (declType) (declProc) (defProc) (declProp)] @scope
(declField name: (identifier) @declaration @definition @signature)
(declVar name: (identifier) @declaration @definition @signature)
(declArg name: (identifier) @declaration @definition @signature)
[(root) (unit) (program) (library) (declType) (declProc) (defProc)
 (declProp) (declField) (declVar) (declArg) (declConst) (declHelper)
 (declVariant) (genericTpl) (lambda) (exprCall) (pp)] @expression
(identifier) @reference
(moduleName) @reference
(exprDot) @reference
(comment) @comment
(literalString) @string
