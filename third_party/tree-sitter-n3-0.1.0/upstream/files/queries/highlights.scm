(string) @string

(lang_tag) @type

[
  "_:"
  "<"
  ">"
  (pname_ns)
] @module

[
  (iri_reference)
  (prefixed_name)
] @variable

(blank_node_label) @variable

"a" @variable.builtin
"=" @variable.builtin
"=>" @variable.builtin
"<=" @variable.builtin
"is" @variable.builtin
"of" @variable.builtin

(integer) @number

[
  (decimal)
  (double)
] @number.float

(boolean_literal) @boolean

[
  "BASE"
  "PREFIX"
  "@prefix"
  "@base"
] @keyword

[
  "."
  ","
  ";"
] @punctuation.delimiter

[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
  (anon)
] @punctuation.bracket

(comment) @comment @spell

(echar) @string.escape

(rdf_literal
  "^^" @type
  datatype:
    (_
      [
        "<"
        ">"
        (pname_ns)
      ] @type) @type)
