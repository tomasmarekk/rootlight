; DTD retains written names, declaration contexts and complete literal parents.
; These captures do not assert evaluated entities or structural declaration kinds.
(extSubset) @root @module
[(elementdecl) (AttlistDecl) (AttDef) (GEDecl) (PEDecl) (NotationDecl)
 (conditionalSect)] @signature

; No synthetic structural declarations, definitions or scopes are advertised.
(Name) @reference
(PITarget) @reference
[(AttValue) (EntityValue) (PI) (TextDecl) (SystemLiteral) (PubidLiteral)
 (CharRef) (elementdecl) (AttlistDecl) (AttDef) (GEDecl) (PEDecl)
 (NotationDecl) (conditionalSect)] @string
(Comment) @comment @documentation
