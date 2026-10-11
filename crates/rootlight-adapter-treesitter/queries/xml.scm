; Physical XML source owners are projected by the XML admission helper.
; Full literal parents remain source evidence, not namespace or entity validity.
(document) @root @module
(element) @declaration @definition @signature
(Attribute) @declaration @definition @signature

; Required contexts survive bounded identity extraction; these are not headers.
[(elementdecl) (AttlistDecl) (AttDef) (GEDecl) (PEDecl) (NotationDecl)
 (doctypedecl)] @signature

; Optional source evidence is separate from the required context patterns.
(Name) @reference
(PITarget) @reference
[(AttValue) (PseudoAttValue) (EntityValue) (PI) (StyleSheetPI) (XmlModelPI)
 (XMLDecl) (SystemLiteral) (PubidLiteral) (CharRef) (CharData) (CDSect)
 (elementdecl) (AttlistDecl) (AttDef) (GEDecl) (PEDecl) (NotationDecl)
 (doctypedecl)] @string
(Comment) @comment @documentation
