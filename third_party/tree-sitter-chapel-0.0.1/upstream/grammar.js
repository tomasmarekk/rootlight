const PREC = {
  assign:         2 ,
  type:           3 ,
  identifier:     4 ,
  binary:         5 ,
  or:             5 ,
  and:            6 ,
  comparative:    7 ,
  bitwise:        8 ,
  bitshift:       9 ,
  by:             10,
  count:          11,
  addition:       12,
  multiply:       13,
}

module.exports = grammar({
  name: 'Chapel',
  rules: {
    // note: order of rules in this file controls matching precedence
    source_file: $ => repeat(
      choice(
        $._statement,
        $._comment,
      )
    ),

    _comment: $ => choice(
      $.inline_comment,
      $.block_comment,
    ),

    // http://stackoverflow.com/questions/13014947/regex-to-match-a-c-style-multiline-comment/36328890#36328890
    inline_comment: $ => token(seq(
      '//',
      /(\\(.|\r?\n)|[^\\\n])*/
    )),

    block_comment: $ => token(seq(
      '/*',
      /[^*]*\*+([^/*][^*]*\*+)*/,
      '/'
    )),

    argument_list: $ => seq(
      '(',
      //optional($.formals),
      ')',
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

    _statement: $ => choice(
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
        //$.member_access_expression,
        //$.call_expression,
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
      $.literal_expression,
      $.variable_expression,
      //$.call_expression,
      $.type_expression,
      $.lvalue_expression,
      // $.binary_expression,
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

    type_expression: $ => prec.left(choice(
      $.primitive_type,
      //$.enum_type,
      $._expression,
    )),

    lvalue_expression: $ => prec(PREC.assign,choice(
      $.variable_expression,
      //$.member_access_expression,
      //$.call_expression,
      //$.parenthesized_expression,
    )),

    // binary_expression: $ => prec(PREC.binary,seq(
    //   $._expression,
    //   $.binary_operator,
    //   $._expression,
    // )),

    bool_literal: $ => choice(
      'true',
      'false',
    ),

    integer_literal: $ => prec.left(choice(
      $._digits,
      // TODO: add other choices here
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
      /[a-zA-z_]/,
      optional(/[a-zA-z\d_$]*/),
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

    // binary_operator: $ => {
    //   const table = [
    //     ['+',  PREC.addition],
    //     ['-',  PREC.addition],
    //     ['*',  PREC.multiply],
    //     ['/',  PREC.multiply],
    //     ['%',  PREC.multiply],
    //     ['**', PREC.multiply],
    //     ['&',  PREC.bitwise],
    //     ['|',  PREC.bitwise],
    //     ['^',  PREC.bitwise],
    //     ['<<', PREC.bitshift],
    //     ['>>', PREC.bitshift],
    //     ['&&', PREC.and],
    //     ['||', PREC.or],
    //     ['==', PREC.comparative],
    //     ['!=', PREC.comparative],
    //     ['<=', PREC.comparative],
    //     ['>=', PREC.comparative],
    //     ['<',  PREC.comparative],
    //     ['>',  PREC.comparative],
    //     ['by', PREC.by],
    //     ['#', PREC.count],
    //   ];

    //   return choice(
    //     ...table.map(([operator, precedence]) =>
    //       prec.left(
    //         precedence,
    //         seq(
    //           $._expression,
    //           operator,
    //           $._expression,
    //         ))));
    // },

  }
});
