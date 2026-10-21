// Finite Chapel structural grammar inputs; generated outputs are deliberately absent.
// The source map binds these extensions to preserved MIT and Apache-2.0 inputs.
const PREC = {
  assign:         2 ,
  type:           3 ,
  identifier:     4 ,
  binary:         5 ,
  or:             4 ,
  and:            5 ,
  equality:       6 ,
  comparative:    7 ,
  range:          8 ,
  addition:       9 ,
  bit_or:         10,
  bit_xor:        11,
  bit_and:        12,
  bitshift:       13,
  multiply:       15,
  postfix:        18,
}

const KEYWORDS = ["_", "align", "as", "atomic", "begin", "bool", "borrowed", "break", "by", "bytes", "catch", "class", "cobegin", "coforall", "complex", "config", "const", "continue", "defer", "deinit", "delete", "dmapped", "do", "domain", "else", "enum", "except", "export", "extern", "false", "for", "forall", "foreach", "forwarding", "if", "imag", "implements", "in", "index", "init", "inline", "inout", "int", "interface", "iter", "label", "let", "lifetime", "local", "locale", "manage", "module", "new", "nil", "noinit", "on", "only", "operator", "otherwise", "out", "override", "owned", "param", "postinit", "private", "proc", "prototype", "public", "range", "real", "record", "reduce", "ref", "require", "return", "scan", "select", "serial", "shared", "single", "sparse", "string", "subdomain", "super", "sync", "then", "these", "this", "throw", "throws", "true", "try", "type", "uint", "union", "unmanaged", "use", "var", "void", "when", "where", "while", "with", "yield", "zip", "lambda", "pragma", "primitive"];

// Reachable supported rules already make these official keywords terminals.
const SUPPORTED_KEYWORDS = ["_", "bool", "borrowed", "bytes", "class", "complex", "const", "enum", "false", "for", "imag", "in", "inline", "int", "module", "owned", "param", "private", "proc", "public", "real", "record", "ref", "return", "shared", "string", "true", "uint", "unmanaged", "var", "void"];
const UNSUPPORTED_KEYWORDS = KEYWORDS.filter(keyword => !SUPPORTED_KEYWORDS.includes(keyword));

module.exports = grammar({
  name: 'Chapel',
  extras: $ => [/\s/, $.inline_comment, $.block_comment],
  externals: $ => [$.block_comment],
  word: $ => $.identifier,
  reserved: {
    global: _ => KEYWORDS,
  },
  rules: {
    source_file: $ => repeat($._statement),

    inline_comment: $ => token(seq('//', /[^\r\n]*/)),

    argument_list: $ => seq(
      '(',
      optional(seq($._expression, repeat(seq(',', $._expression)))),
      ')',
    ),

    module_declaration: $ => seq(
      optional($.privacy_specifier), 'module',
      field('name', $.identifier), field('body', $.block_statement),
    ),

    procedure_declaration: $ => seq(
      optional($.privacy_specifier), optional('inline'), 'proc',
      field('name', $.identifier), field('parameters', $.formal_list),
      optional(seq(':', field('return_type', $.type_expression))),
      field('body', $.block_statement),
    ),

    formal_list: $ => seq(
      '(', optional(seq($.formal_parameter, repeat(seq(',', $.formal_parameter)))), ')',
    ),

    formal_parameter: $ => seq(
      field('name', $.identifier),
      optional(seq(':', field('type', $.type_expression))),
    ),

    for_statement: $ => seq(
      'for', field('index', $.identifier), 'in',
      field('iterable', $._expression), field('body', $.block_statement),
    ),

    privacy_specifier: $ => choice(
      'private',
      'public',
    ),

    proc_or_iter: $ => choice(
      'proc',
      'iter',
    ),

    procedure_kind: $ => choice(
      'inline',
      'export',
      'extern',
      'override',
    ),

    // This recovery node carries unsupported syntax, never declaration or binding proof.
    unsupported_keyword_statement: $ => seq(choice(...UNSUPPORTED_KEYWORDS), ';'),

    _statement: $ => choice(
      $.unsupported_keyword_statement,
      $.module_declaration,
      $.procedure_declaration,
      $.for_statement,
      $.block_statement,
      $.expression_statement,
      $.assignment_statement,
      $.return_statement,
      $.variable_declaration_statement,
      //$.if_statement
    ),

    block_statement: $ => seq(
      '{',
      optional($.statements),
      '}'
    ),

    expression_statement: $ => seq(
      choice(
        $.variable_expression,
        $.member_access_expression,
        $.call_expression,
        //$.new_expression,
        //$.let_expression,
      ),
      ';',
    ),

    assignment_statement: $ => prec(PREC.assign,seq(
      $.lvalue_expression,
      $.assignment_operator,
      $._expression,
      ';', // TODO: final ';' not in docs, but needed to avoid error on "a = 6;"
    )),

    statements: $ => seq(
      $._statement,
      optional($.statements),
    ),

    variable_declaration_statement: $ => seq(
      optional($.privacy_specifier),
      //optional($.config-extern-or-export),
      $.variable_kind,
      $.variable_declaration_list,
      ';',
    ),

    variable_declaration_list: $ => choice(
      $.variable_declaration,
      seq(
        $.variable_declaration,
        ',',
        $.variable_declaration_list,
      ),
    ),

    variable_declaration: $ => seq(
      $.identifier_list,
      optional($.type_part),
      optional($.initialization_part),
    ),

    type_part: $ => prec(PREC.type,seq(
      ':',
      $.type_expression,
    )),

    initialization_part: $ => prec(PREC.assign,seq(
      '=',
      $._expression,
    )),

    _expression: $ => choice(
      $.literal_expression, $.variable_expression, $.primitive_type,
      $.call_expression, $.member_access_expression, $.parenthesized_expression,
      $.range_expression, $.binary_expression,
    ),

    call_expression: $ => prec.left(PREC.postfix, seq(
      field('function', choice(
        $.variable_expression, $.primitive_type, $.member_access_expression,
        $.call_expression, $.parenthesized_expression,
      )), field('arguments', $.argument_list),
    )),

    member_access_expression: $ => prec.left(PREC.postfix, seq(
      field('object', $._expression), '.', field('property', $.identifier),
    )),

    parenthesized_expression: $ => seq('(', $._expression, ')'),

    range_expression: $ => prec.left(PREC.range, seq(
      field('start', $._expression), '..', field('end', $._expression),
    )),

    binary_expression: $ => choice(
      ...[
        ['||', PREC.or], ['&&', PREC.and],
        ['==', PREC.equality], ['!=', PREC.equality],
        ['<', PREC.comparative], ['>', PREC.comparative],
        ['<=', PREC.comparative], ['>=', PREC.comparative],
        ['+', PREC.addition], ['-', PREC.addition],
        ['|', PREC.bit_or], ['^', PREC.bit_xor], ['&', PREC.bit_and],
        ['<<', PREC.bitshift], ['>>', PREC.bitshift],
        ['*', PREC.multiply], ['/', PREC.multiply], ['%', PREC.multiply],
      ].map(([operator, precedence]) => prec.left(precedence, seq(
        field('left', $._expression), operator, field('right', $._expression),
      ))),
    ),

    literal_expression: $ => choice(
      $.bool_literal,
      $.integer_literal,
      $.real_literal,
      //$.imaginary_literal,
      $.string_literal,
      //$.bytes_literal,
      //$.range_literal,
      //$.domain_literal,
      //$.array_literal,
    ),

    variable_expression: $ => $.identifier,

    type_expression: $ => choice(
      $.primitive_type, $.variable_expression, $.call_expression,
      $.member_access_expression, $.parenthesized_expression,
    ),

    lvalue_expression: $ => prec(PREC.assign,choice(
      $.variable_expression,
      $.member_access_expression,
    )),

    bool_literal: $ => choice(
      'true',
      'false',
    ),

    integer_literal: $ => token(choice(
      /[0-9][0-9_]*/,
      /0[xX][0-9a-fA-F][0-9a-fA-F_]*/,
      /0[oO][0-7][0-7_]*/,
      /0[bB][01][01_]*/,
    )),

    real_literal: $ => choice(
      seq(
        optional($._digits),
        '.',
        $._digits,
        optional($._exponent_part),
      ),
      seq(
        $._digits,
        optional('.'),
        $._exponent_part,
      ),
      // TODO: hexadecimal
    ),

    _exponent_part: $ => seq(
      choice('e','E'),
      optional($._sign),
      $._digits,
    ),

    string_literal: $ => choice(
      $.interpreted_string_literal,
      //$.uninterpreted_string_literal,
    ),

    interpreted_string_literal: $ => choice(
      seq(
        '"',
        optional($._double_quote_delimited_characters),
        '"',
      ),
      seq(
        '\'',
        optional($._single_quote_delimited_characters),
        '\'',
      ),
    ),

    _double_quote_delimited_characters: $ => choice(
      seq(
        $._string_character,
        optional($._double_quote_delimited_characters),
      ),
      seq(
        '\'',
        optional($._double_quote_delimited_characters),
      )
    ),

    _single_quote_delimited_characters: $ => choice(
      seq(
        $._string_character,
        optional($._single_quote_delimited_characters),
      ),
      seq(
        '"',
        optional($._single_quote_delimited_characters),
      )
    ),

    _string_character: $ => choice(
      /[^'"\n]/,
      //$.simple_escape_character,
      //$.hexadecimal_escape_character,
    ),

    primitive_type: $ => choice(
      'void',
      'nothing',
      'bool',
      'int', // TODO: make into seq(...,optional(...)) when primitive-type-parameter-part added
      'uint',
      'real',
      'imag',
      'complex',
      'string',
      'bytes',
      'enum',
      'record',
      'class',
      'owned',
      'shared',
      'unmanaged',
      'borrowed',
    ),

    variable_kind: $ => choice(
      'param',
      'const',
      'var',
      'ref',
      'const ref',
    ),

    return_statement: $ => seq(
      'return',
      optional($._expression),
      ';',
    ),

    identifier: $ => token(seq(
      /[a-zA-Z_]/,
      optional(/[a-zA-Z\d_$]*/),
    )),

    // TODO: the above rule for identifier is functional for now, but if
    // tree-sitter #449 is ever resolved, use the following instead:
    //identifier: $ => token(seq(
    //  $._letter_or_underscore,
    //  optional($._legal_identifier_chars),
    //),

    identifier_list: $ => prec.left(PREC.identifier,choice(
      $.identifier,
      seq(
        $.identifier,
        ',',
        $.identifier_list,
      ),
      //$.tuple_grouped_identifier_list,
      //seq(
      //  $.tuple_grouped_identifier_list,
      //  $.identifier_list,
      //),
    )),

    _legal_identifier_chars: $ => prec.left(PREC.identifier,seq(
      $._legal_identifier_char,
      optional($._legal_identifier_chars),
    )),

    _legal_identifier_char: $ => choice(
      $._letter_or_underscore,
      $._digit,
      '$',
    ),

    _letter_or_underscore: $ => choice(
      $._letter,
      '_',
    ),

    _anything: $ => /.*/,
    _letter: $ => /[a-zA-Z]/,
    _digit: $ => /[\d]/,
    _binary_digit: $ => /[01]/,

    _digits: $ => prec.left(choice(
      $._digit,
      seq(
        $._digit,
        $._separator_digits,
      ),
    )),

    _separator_digits: $=> prec.left(choice(
      $._digit,
      '_',
      seq(
        $._digit,
        $._separator_digits,
      ),
      seq(
        '_',
        $._separator_digits,
      ),
    )),

    _sign: $ => choice(
      '+',
      '-',
    ),

    assignment_operator: $ => choice(
      '=',
      '+=',
      '-=',
      '*=',
      '/=',
      '%=',
      '**=',
      '&=',
      '|=',
      '^=',
      '&&=',
      '||=',
      '<<=',
      '>>=',
    ),

  }
});
