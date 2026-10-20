; Modelica written function/input captures require native owner admission.
; General class, equation and runtime semantics remain explicit source gaps.
(StoredDefinition) @root @module @scope
(ClassDefinition) @declaration @definition @signature
(ClassDefinition) @scope
(ComponentClause
  componentDeclaration: (ComponentDeclaration
    declaration: (Declaration
      identifier: (IDENT) @declaration @definition)))
(ComponentReferencePart identifier: (IDENT)) @reference

; Keep unsupported constructs separate from identity captures.
[(ClassDefinition) (ComponentClause) (ShortClassDefinition)
 (ExtendsClause) (SimpleImportClause) (CompoundImportClause)
 (UnqualifiedImportClause) (FunctionCall) (ForStatement) (ForEquation)
 (WhenStatement) (WhenEquation) (AnnotationClause) (ERROR)] @expression
[(BLOCK_COMMENT) (LINE_COMMENT)] @comment
(STRING) @string
