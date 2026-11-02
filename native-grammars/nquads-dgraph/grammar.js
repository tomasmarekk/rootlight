// Physical Dgraph statements retain native identifiers and default-value spelling.
// Namespace, facet, blank-label and typed-value syntax remains outside this
// bounded grammar; standard N-Quads rules are maintained in a separate package.

const IDENTIFIER_ESCAPE = choice(/\\u[0-9A-Fa-f]{4}/, /\\U[0-9A-Fa-f]{8}/);

module.exports = grammar({
  name: 'nquads_dgraph',

  externals: $ => [$.string, $.comment],

  extras: $ => [/[ \t]/, $.comment],

  rules: {
    document: $ => seq(
      repeat(seq(optional($.quad), $._end_of_line)),
      optional($.quad),
    ),

    quad: $ => seq(
      field('subject', $.identifier),
      field('predicate', $.identifier),
      field('object', choice($.identifier, $.default_value)),
      '.',
    ),

    identifier: _ => token(seq(
      '<', repeat1(choice(/[^\x00-\x20<>"{}|^`\\]/u, IDENTIFIER_ESCAPE)), '>',
    )),

    default_value: $ => field('value', $.string),
    _end_of_line: _ => /[\r\n]+/,
  },
});
