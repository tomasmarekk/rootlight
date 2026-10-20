; Written JSX containers use native names; expressions keep ordinary JS roles.
; Separate context patterns preserve gaps during identity-only extraction.
(jsx_element open_tag: (jsx_opening_element name: (_) @definition)) @declaration
(jsx_self_closing_element name: (_) @definition) @declaration
(jsx_attribute . [(property_identifier) (jsx_namespace_name)] @definition) @declaration
[(jsx_element) (jsx_self_closing_element)] @expression
(jsx_attribute) @expression
