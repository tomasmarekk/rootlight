// [X]  See section "4.7 Grammar" in https://w3c.github.io/N3/spec/#grammar for
//      corresponding rule x.

// [35]
const UCHAR = /(\\u[0-9A-Fa-f]{4}|\\U[0-9A-Fa-f]{8})/

// [155s]
const EXPONENT = [
  /[eE]/,
  /[+-]?/,
  /[0-9]+/
]

// [162s]
const WS = [
  /\x20/,
  /\x09/,
  /\x0D/,
  /\x0A/
]

// [163s]
const PN_CHARS_BASE = [
  /[A-Z]/,
  /[a-z]/,
  /[\u00C0-\u00D6]/,
  /[\u00D8-\u00F6]/,
  /[\u00F8-\u02FF]/,
  /[\u0370-\u037D]/,
  /[\u037F-\u1FFF]/,
  /[\u200C-\u200D]/,
  /[\u2070-\u218F]/,
  /[\u2C00-\u2FEF]/,
  /[\u3001-\uD7FF]/,
  /[\uF900-\uFDCF]/,
  /[\uFDF0-\uFFFD]/,
  /[\u{10000}-\u{EFFFF}]/u
]

// [165s]
const PN_CHARS_U = PN_CHARS_BASE.concat('_')

// [167s]
const PN_CHARS = PN_CHARS_U.concat([
  '-',
  /[0-9]/,
  /[\u00B7]/,
  /[\u0300-\u036F]/,
  /[\u203F-\u2040]/
])

// [172s]
const HEX = [
  /[0-9]/,
  /[A-F]/,
  /[a-f]/
]

// [173s]
const PN_LOCAL_ESC = [
  '_',
  '~',
  '.',
  '-',
  '!',
  '$',
  '&',
  "'",
  '(',
  ')',
  '*',
  '+',
  ',',
  ';',
  '=',
  '/',
  '?',
  '#',
  '@',
  '%'
].map(char => '\\' + char)

String.prototype.toCaseInsensitiv = function () {
  return alias(
    token(new RegExp(
      this
        .split('')
        .map(letter => `[${letter}${letter.toLowerCase()}]`)
        .join('')
    )),
    this
  )
}

module.exports = grammar({
  name: 'n3',

  extras: $ => [
    $.comment,
    /\s/
  ],

  word: $ => $.pn_prefix,

  rules: {

    // [1]
    n3_doc: $ => repeat(choice(
      seq($.n3_statement, '.'),
      $.sparql_directive)
    ),

    comment: $ => token(prec(-1, /#.*/)),

    // [2]
    n3_statement: $ => choice(
      $.n3_directive,
      $.triples
    ),

    // [3]
    n3_directive: $ => choice(
      $.prefix_id,
      $.base
    ),

    // [4]
    sparql_directive: $ => choice(
      $.sparql_base,
      $.sparql_prefix
    ),

    // [5]
    sparql_base: $ => seq(
      'BASE'.toCaseInsensitiv(),
      $.iri_reference,
    ),

    // [6]
    sparql_prefix: $ => seq(
      'PREFIX'.toCaseInsensitiv(),
      $.pname_ns,
      $.iri_reference,
    ),

    // [7]
    prefix_id: $ => seq(
      '@prefix',
      $.pname_ns,
      $.iri_reference,
    ),

    // [8]
    base: $ => seq(
      '@base',
      $.iri_reference,
    ),


    // [9]
    triples: $ => seq(
      $.subject,
      optional($.predicate_object_list)
    ),

    // [10]
    predicate_object_list: $ => seq(
      $.verb,
      $.object_list,
      repeat(
        seq(';', optional(seq($.verb, $.object_list)))
      ),
    ),

    // [11]
    object_list: $ => seq(
      $.object,
      repeat(seq(
        ',',
        $.object
      ))
    ),

    // [12]
    verb: $ => choice(
      $.predicate,
      'a',
      seq('has', $.expression),
      seq('is', $.expression, 'of'),
      '=',
      '<=',
      '=>'
    ),

    // [13]
    subject: $ => $.expression,

    // [14]
    predicate: $ => choice(
      $.expression,
      seq('<-', $.expression)
    ),

    // [15]
    object: $ => $.expression,

    // [16]
    expression: $ => $.path,

    // [17]
    path: $ => seq(
      $.path_item,
      optional(
        choice(
          seq('!', $.path),
          seq('^', $.path)
        )
      )
    ),

    // [18]
    path_item: $ => choice(
      $._iri,
      $._blank_node,
      $.quick_var,
      $.collection,
      $.blank_node_property_list,
      $.iri_property_list,
      $._literal,
      $.formula
    ),

    // [19]
    _literal: $ => choice(
      $.rdf_literal,
      $.numeric_literal,
      $.boolean_literal
    ),

    // [20]
    blank_node_property_list: $ => seq(
      '[',
      repeat(choice(...WS)),
      $.predicate_object_list,
      ']'
    ),

    // [21]
    iri_property_list: $ => seq(
      $.ipl_start,
      $._iri,
      $.predicate_object_list,
      ']'
    ),

    // [22]
    collection: $ => seq(
      '(',
      repeat($.expression),
      ')'
    ),

    // [23]
    formula: $ => seq(
      '{',
      optional($.formula_content),
      '}'
    ),

    // [24]
    formula_content: $ => choice(
      seq(
        $.n3_statement,
        optional(seq(
          '.',
          optional($.formula_content)
        )),
      ),
      seq(
        $.sparql_directive,
        optional($.formula_content)
      )
    ),

    // [25]
    numeric_literal: $ => choice(
      $.double,
      $.decimal,
      $.integer
    ),

    // [26]
    rdf_literal: $ => seq(
      field('value', $.string),
      optional(choice(
        field('lang', $.lang_tag),
        field('datatype', seq('^^', $._iri))
      ))
    ),

    // [27]
    _iri: $ => choice(
      $.iri_reference,
      $.prefixed_name
    ),

    // [28]
    prefixed_name: $ => choice(
      $.pname_ns,
      $.pname_ln
    ),

    // [29]
    _blank_node: $ => choice(
      $.blank_node_label,
      $.anon
    ),

    // [30]
    // [36]
    quick_var: $ => seq(
      "?",
      $.pn_local
    ),

    // [31]
    boolean_literal: $ => choice(
      'true',
      'false'
    ),

    // [32]
    string: $ => choice(
      $._string_literal_quote,
      $._string_literal_single_quote,
      $._string_literal_long_quote,
      $._string_literal_long_single_quote,
    ),

    // [33]
    ipl_start: $ => seq(
      '[',
      repeat(choice(...WS)),
      'id'
    ),

    // [139s]
    iri_reference: $ => seq(
      '<',
      token.immediate(repeat(choice(
        /([^<>"{}|^`\\\x00-\x20])/,
        UCHAR
      ))),
      token.immediate(
        '>'
      )
    ),

    // [140s]
    pname_ns: $ => seq(
      optional($.pn_prefix),
      ':'
    ),

    // [141s]
    pname_ln: $ => seq(
      $.pname_ns,
      $.pn_local
    ),

    // [142s]
    blank_node_label: $ => seq(
      '_:',
      token.immediate(seq(
        choice(
          ...PN_CHARS_U,
          /[0-9]/
        ),
        optional(seq(
          repeat(choice(
            ...PN_CHARS,
            '.'
          )),
          choice(...PN_CHARS)
        ))
      ))
    ),

    // [145s]
    lang_tag: $ => token(seq(
      '@',
      /[a-zA-Z]+/,
      repeat(seq('-', /[a-zA-Z0-9]+/))
    )),
    // [146s]
    integer: $ => token(/[+-]?[0-9]+/),

    // [147s]
    decimal: $ => token(seq(/[+-]?/, /[0-9]*/, '.', /[0-9]+/)),

    // [148s]
    double: $ => token(seq(
      /[+-]?/,
      choice(
        seq(/[0-9]+/, '.', /[0-9]*/, seq(...EXPONENT)),
        seq('.', /[0-9]+/, seq(...EXPONENT)),
        seq(/[0-9]+/, seq(...EXPONENT))
      ))
    ),

    // [156s]
    _string_literal_quote: $ => seq(
      '"',
      repeat(choice(
        /[^\x22\x5C\x0A\x0D]/,
        $.echar,
        UCHAR
      )),
      '"',
    ),

    // [157s]
    _string_literal_single_quote: $ => seq(
      "'",
      repeat(choice(
        /[^\x27\x5C\x0A\x0D]/,
        $.echar,
        UCHAR
      )),
      "'"
    ),

    // [158s]
    _string_literal_long_single_quote: $ => seq(
      "'''",
      repeat(seq(
        optional(choice(
          "'",
          "''",
        )),
        choice(
          /[^'\\]/,
          $.echar,
          UCHAR
        )
      )),
      "'''",
    ),

    // [159s]
    _string_literal_long_quote: $ => seq(
      '"""',
      repeat(seq(
        optional(choice(
          '"',
          '""',
        )),
        choice(
          /[^"\\]/,
          $.echar,
          UCHAR
        )
      )),
      '"""',
    ),

    // [160s]
    echar: $ => /\\[tbnrf\\"']/,

    // [163s]
    anon: $ => token(seq(
      '[',
      repeat(choice(...WS)),
      ']'
    )),

    // [167s]
    pn_prefix: $ => token(seq(
      choice(...PN_CHARS_BASE),
      optional(seq(
        repeat(choice(
          ...PN_CHARS,
          '.'
        )),
        choice(...PN_CHARS)
      ))
    )),

    // [168s]
    pn_local: $ => token.immediate(seq(
      choice(
        ...PN_CHARS_U,
        ':',
        /[0-9]/,
        seq('%', choice(...HEX), choice(...HEX)),
        ...PN_LOCAL_ESC
      ),
      optional(seq(
        repeat(choice(
          ...PN_CHARS,
          '.',
          ':',
          seq('%', choice(...HEX), choice(...HEX)),
          ...PN_LOCAL_ESC
        )),
        choice(
          ...PN_CHARS,
          ':',
          seq('%', choice(...HEX), choice(...HEX)),
          ...PN_LOCAL_ESC
        )
      ))
    )),
  }
})
