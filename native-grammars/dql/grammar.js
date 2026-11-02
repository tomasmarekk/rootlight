// Native DQL query ownership follows the retained Dgraph lexer/parser contract.
// Commas separate arguments, while selection and root lists also permit omission.

const ci = text => new RegExp(text.split('').map(c => `[${c.toLowerCase()}${c.toUpperCase()}]`).join(''));
const separated = rule => seq(rule, repeat(seq(',', rule)));
const GENERATORS = [
  'regexp', 'anyofterms', 'allofterms', 'alloftext', 'anyoftext', 'ngram',
  'has', 'uid_in', 'anyof', 'allof', 'match', 'similar_to',
  'near', 'contains', 'within', 'intersects', 'eq', 'le', 'ge', 'gt', 'lt', 'between',
];

module.exports = grammar({
  name: 'dql',
  word: $ => $.identifier,
  externals: $ => [$.string, $.comment],
  extras: $ => [/[ \t\r\n]/, $.comment],
  conflicts: $ => [[$.selection_predicate]],

  rules: {
    document: $ => $.query_operation,

    query_operation: $ => seq(
      optional(seq(ci('query'), optional(field('name', $.identifier)), optional($.variable_definitions))),
      field('body', $.root_selection),
    ),

    variable_definitions: $ => seq('(', separated($.variable_definition), ')'),
    variable_definition: $ => seq(
      field('name', $.variable_reference), ':', field('type', $.identifier),
      optional(choice('!', seq('=', field('default', $._value)))),
    ),
    variable_reference: $ => seq('$', field('name', $.identifier)),
    variable_binding: $ => seq(field('name', $.identifier), ci('as')),
    output_alias: $ => seq(field('name', $.identifier), ':'),

    root_selection: $ => seq('{', repeat1(seq(choice($.root_query, $.aggregation_root), optional(','))), '}'),
    root_query: $ => seq(
      optional(field('binding', $.variable_binding)), field('name', $.identifier),
      field('arguments', $.root_arguments), repeat($._directive),
      field('body', $.selection_set),
    ),
    root_arguments: $ => seq(
      '(', repeat(seq($.argument, ',')), field('generator', $.generator_argument),
      repeat(seq(',', $.argument)), ')',
    ),
    generator_argument: $ => seq('func', ':', field('value', $.root_function)),
    root_function: $ => choice(
      seq(field('name', alias(ci('uid'), $.function_name)), '(', optional(separated($._value)), ')'),
      seq(field('name', alias(ci('type'), $.function_name)), '(', $._value, ')'),
      seq(field('name', alias(choice(...GENERATORS.map(ci)), $.function_name)),
        '(', field('arguments', $.function_arguments), ')'),
    ),
    argument: $ => seq(field('name', $.identifier), ':', field('value', $._value)),

    aggregation_root: $ => seq(
      field('name', $.identifier), '(', ')',
      field('body', $.aggregation_selection),
    ),
    aggregation_selection: $ => seq('{', repeat1(seq($.aggregate_selection, optional(','))), '}'),
    aggregate_selection: $ => seq(
      optional($.output_alias), field('name', alias(choice(ci('sum'), ci('avg'), ci('min'), ci('max')), $.function_name)),
      '(', $.nested_function, ')',
    ),

    selection_set: $ => seq('{', repeat1(seq($._selection, optional(','))), '}'),
    _selection: $ => choice($.field_selection, $.count_selection, $.value_selection),
    field_selection: $ => seq(
      optional(field('binding', $.variable_binding)), optional(field('alias', $.output_alias)),
      field('predicate', $.selection_predicate), optional(field('arguments', $.field_arguments)),
      repeat($._directive), optional(field('body', $.selection_set)),
    ),
    count_selection: $ => seq(
      optional(field('binding', $.variable_binding)), optional(field('alias', $.output_alias)),
      field('name', alias(ci('count'), $.function_name)),
      '(', field('predicate', $.predicate_name), ')',
    ),
    value_selection: $ => seq(
      optional(field('alias', $.output_alias)),
      field('name', alias(ci('val'), $.function_name)),
      '(', field('variable', $.identifier), ')',
    ),
    field_arguments: $ => seq('(', separated($.argument), ')'),

    predicate_name: $ => seq(optional(field('reverse', '~')), field('name', choice($.identifier, $.iri))),
    predicate: $ => seq($.predicate_name, optional(field('language', $.function_language))),
    selection_predicate: $ => seq($.predicate_name, optional(field('language', $.language_suffix))),
    function_language: $ => seq('@', choice('.', $.language_tag)),
    language_suffix: $ => seq('@', choice('.', '*', separatedLanguages($))),
    language_tag: _ => token(/[A-Za-z][A-Za-z0-9-]*/),

    function_call: $ => seq(
      field('name', alias($.identifier, $.function_name)),
      '(', field('arguments', $.function_arguments), ')',
    ),
    function_arguments: $ => seq(
      field('predicate', choice($.predicate, $.nested_function)), repeat(seq(',', $._value)),
    ),
    nested_function: $ => seq(
      field('name', alias(choice(ci('val'), ci('count'), ci('len'), ci('uid')), $.function_name)),
      '(', field('argument', $.predicate), ')',
    ),
    _value: $ => choice($.function_call, $.nested_function, $.variable_reference, $.predicate,
      $.string, $.number, $.boolean, $.list, $.regex),
    list: $ => seq('[', optional(separated($._value)), ']'),
    boolean: _ => choice(ci('true'), ci('false')),
    number: _ => token(/[+-]?(?:0[xX][0-9A-Fa-f]+|[0-9]+(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?)/),
    identifier: _ => /[A-Za-z_][A-Za-z_0-9.-]*/,
    iri: _ => token(seq('<', repeat1(/[^\x00-\x20<>"{}|^`\\]/u), '>')),
    regex: _ => token(seq('/', repeat(choice(/[^/\\\r\n]/, /\\[^\r\n]/)), '/', optional('i'))),

    _directive: $ => choice($.filter_directive, $.flag_directive),
    filter_directive: $ => seq('@', field('name', alias(token(prec(1, ci('filter'))), $.directive_name)),
      '(', optional($._filter_expression), ')'),
    flag_directive: $ => seq('@', field('name', alias(token(prec(1, choice(
      ci('normalize'), ci('cascade'), ci('ignorereflex'),
    ))), $.directive_name))),
    _filter_expression: $ => choice($.function_call, $.parenthesized_expression, $.unary_expression, $.binary_expression),
    parenthesized_expression: $ => seq('(', $._filter_expression, ')'),
    unary_expression: $ => prec(3, seq(field('operator', alias(token(prec(1, ci('not'))), $.filter_operator)), field('argument', $._filter_expression))),
    binary_expression: $ => choice(
      prec.left(1, seq(field('left', $._filter_expression), field('operator', alias(token(prec(1, ci('or'))), $.filter_operator)), field('right', $._filter_expression))),
      prec.left(2, seq(field('left', $._filter_expression), field('operator', alias(token(prec(1, ci('and'))), $.filter_operator)), field('right', $._filter_expression))),
    ),
  },
});

function separatedLanguages($) {
  return seq($.language_tag, repeat(seq(':', $.language_tag)));
}
