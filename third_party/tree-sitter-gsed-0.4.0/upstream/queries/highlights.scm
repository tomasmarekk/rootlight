(command_name) @keyword

(comment_text) @comment

(directive_text) @keyword.directive

(label_text) @label

(filename_text) @string.special.path

(shell_text) @string

(version_text) @string

(number) @number

(last_line_address) @constant.builtin

[
  (flag)
  (address_modifier)
] @keyword.modifier

(address_operator) @operator

(negation
  "!" @operator)

(duplicate_negation
  "!" @operator)

(block
  [
    "{"
    "}"
  ] @punctuation.bracket)

(unmatched_closing_brace
  "}" @punctuation.bracket)

[
  (delimiter)
  (command_separator)
  (address_separator)
  (interval_separator)
] @punctuation.delimiter

[
  (group_open)
  (group_close)
  (interval_open)
  (interval_close)
  (bracket_open)
  (bracket_close)
  (compound_marker)
  (class_open)
  (class_close)
  (collating_open)
  (collating_close)
  (equivalence_open)
  (equivalence_close)
] @punctuation.bracket

(range_operator) @operator

(regex_operator) @operator

(wildcard
  (regex_operator) @character.special)

(regex_character_class
  (regex_operator) @character.special)

[
  (class_name_text)
  (collating_text)
] @character.special

(regex_character
  (literal) @string.regexp)

(bracket_character
  (literal) @string.regexp)

(invalid_interval_character
  (literal) @string.regexp)

(text
  (literal) @string)

(replacement
  (literal) @string)

(translation_string
  (literal) @string)

[
  (regex_reference)
  (replacement_reference)
  (matched_text_reference)
] @string.special.symbol

[
  (escape)
  (character_escape)
  (case_conversion)
] @string.escape

[
  (line_continuation)
  (text_introducer)
] @punctuation.special
