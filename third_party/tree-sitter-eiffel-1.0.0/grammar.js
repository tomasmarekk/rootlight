/**
 * @file Parser for Eiffel programming language
 * @author Ilgiz Mustafin <ilgimustafin@gmail.com>
 * @license MIT
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

const PREC = {
  UNARY: 2, // UNARY > SIGN so that -1 is parsed as (unary (-) (1))
  ACTUALS: 2,
  TYPE: 2,
};

// Retain stable token names while accepting the language's case-insensitive words.
const keyword = ($, word) => alias($['_keyword_' + word], word);

const EAT_SPACES = token.immediate(/[ \t]*/);

const target = $ => choice(
  $.current,
  $.result,
  $.call,
  $.bracket_expression,
  $.manifest_type,
  $.at_cursor,
  seq('(', $.expression, ')')
);

const join1 = (node, separator)  => seq(
  node,
  repeat(seq(separator, node))
);

module.exports = grammar({
  name: 'eiffel',
  extras: $ => [/[ \t\x0b\f\r\n\u00a0\u1680\u2000-\u200a\u202f\u205f\u3000]+/, $.comment],

  externals: $ => [$._free_operator],
  conflicts: $ => [
    // In Tuple_type parameters: 'TUPLE' '[' (identifier_token) ','
    [$.class_name, $.identifier],
    // A note after a field can belong to its class or introduce a routine body.
    [$.feature_declaration],
  ],
  word: $ => $._id,
  reserved: {
    global: $ => [
      $._keyword_across,
      $._keyword_agent,
      $._keyword_alias,
      $._keyword_all,
      $._keyword_and,
      $._keyword_as,
      $._keyword_assign,
      $._keyword_attribute,
      $._keyword_check,
      $._keyword_class,
      $._keyword_convert,
      $._keyword_create,
      $._keyword_Current,
      $._keyword_debug,
      $._keyword_deferred,
      $._keyword_do,
      $._keyword_else,
      $._keyword_elseif,
      $._keyword_end,
      $._keyword_ensure,
      $._keyword_expanded,
      $._keyword_export,
      $._keyword_external,
      $._keyword_False,
      $._keyword_feature,
      $._keyword_from,
      $._keyword_frozen,
      $._keyword_if,
      $._keyword_implies,
      $._keyword_inherit,
      $._keyword_inspect,
      $._keyword_invariant,
      $._keyword_like,
      $._keyword_local,
      $._keyword_loop,
      $._keyword_not,
      $._keyword_note,
      $._keyword_obsolete,
      $._keyword_old,
      $._keyword_once,
      $._keyword_only,
      $._keyword_or,
      $._keyword_Precursor,
      $._keyword_redefine,
      $._keyword_rename,
      $._keyword_require,
      $._keyword_rescue,
      $._keyword_Result,
      $._keyword_retry,
      $._keyword_select,
      $._keyword_separate,
      $._keyword_then,
      $._keyword_True,
      $._keyword_TUPLE,
      $._keyword_undefine,
      $._keyword_unique,
      $._keyword_until,
      $._keyword_variant,
      $._keyword_Void,
      $._keyword_when,
      $._keyword_xor
    ]
  },
  rules: {
    source_file: $ => optional($.class_declaration),
    _keyword_Current: $ => token(new RegExp('Current', 'i')),
    _keyword_False: $ => token(new RegExp('False', 'i')),
    _keyword_NONE: $ => token(new RegExp('NONE', 'i')),
    _keyword_Precursor: $ => token(new RegExp('Precursor', 'i')),
    _keyword_Result: $ => token(new RegExp('Result', 'i')),
    _keyword_TUPLE: $ => token(new RegExp('TUPLE', 'i')),
    _keyword_True: $ => token(new RegExp('True', 'i')),
    _keyword_Void: $ => token(new RegExp('Void', 'i')),
    _keyword_across: $ => token(new RegExp('across', 'i')),
    _keyword_agent: $ => token(new RegExp('agent', 'i')),
    _keyword_alias: $ => token(new RegExp('alias', 'i')),
    _keyword_all: $ => token(new RegExp('all', 'i')),
    _keyword_and: $ => token(new RegExp('and', 'i')),
    _keyword_as: $ => token(new RegExp('as', 'i')),
    _keyword_assign: $ => token(new RegExp('assign', 'i')),
    _keyword_attached: $ => token(new RegExp('attached', 'i')),
    _keyword_attribute: $ => token(new RegExp('attribute', 'i')),
    _keyword_check: $ => token(new RegExp('check', 'i')),
    _keyword_class: $ => token(new RegExp('class', 'i')),
    _keyword_convert: $ => token(new RegExp('convert', 'i')),
    _keyword_create: $ => token(new RegExp('create', 'i')),
    _keyword_debug: $ => token(new RegExp('debug', 'i')),
    _keyword_deferred: $ => token(new RegExp('deferred', 'i')),
    _keyword_detachable: $ => token(new RegExp('detachable', 'i')),
    _keyword_do: $ => token(new RegExp('do', 'i')),
    _keyword_else: $ => token(new RegExp('else', 'i')),
    _keyword_elseif: $ => token(new RegExp('elseif', 'i')),
    _keyword_end: $ => token(new RegExp('end', 'i')),
    _keyword_ensure: $ => token(new RegExp('ensure', 'i')),
    _keyword_expanded: $ => token(new RegExp('expanded', 'i')),
    _keyword_export: $ => token(new RegExp('export', 'i')),
    _keyword_external: $ => token(new RegExp('external', 'i')),
    _keyword_feature: $ => token(new RegExp('feature', 'i')),
    _keyword_from: $ => token(new RegExp('from', 'i')),
    _keyword_frozen: $ => token(new RegExp('frozen', 'i')),
    _keyword_if: $ => token(new RegExp('if', 'i')),
    _keyword_implies: $ => token(new RegExp('implies', 'i')),
    _keyword_inherit: $ => token(new RegExp('inherit', 'i')),
    _keyword_inspect: $ => token(new RegExp('inspect', 'i')),
    _keyword_invariant: $ => token(new RegExp('invariant', 'i')),
    _keyword_like: $ => token(new RegExp('like', 'i')),
    _keyword_local: $ => token(new RegExp('local', 'i')),
    _keyword_loop: $ => token(new RegExp('loop', 'i')),
    _keyword_not: $ => token(new RegExp('not', 'i')),
    _keyword_note: $ => token(new RegExp('note', 'i')),
    _keyword_obsolete: $ => token(new RegExp('obsolete', 'i')),
    _keyword_old: $ => token(new RegExp('old', 'i')),
    _keyword_once: $ => token(new RegExp('once', 'i')),
    _keyword_only: $ => token(new RegExp('only', 'i')),
    _keyword_or: $ => token(new RegExp('or', 'i')),
    _keyword_reference: $ => token(new RegExp('reference', 'i')),
    _keyword_redefine: $ => token(new RegExp('redefine', 'i')),
    _keyword_rename: $ => token(new RegExp('rename', 'i')),
    _keyword_require: $ => token(new RegExp('require', 'i')),
    _keyword_rescue: $ => token(new RegExp('rescue', 'i')),
    _keyword_retry: $ => token(new RegExp('retry', 'i')),
    _keyword_select: $ => token(new RegExp('select', 'i')),
    _keyword_separate: $ => token(new RegExp('separate', 'i')),
    _keyword_some: $ => token(new RegExp('some', 'i')),
    _keyword_then: $ => token(new RegExp('then', 'i')),
    _keyword_undefine: $ => token(new RegExp('undefine', 'i')),
    _keyword_unique: $ => token(new RegExp('unique', 'i')),
    _keyword_until: $ => token(new RegExp('until', 'i')),
    _keyword_variant: $ => token(new RegExp('variant', 'i')),
    _keyword_when: $ => token(new RegExp('when', 'i')),
    _keyword_xor: $ => token(new RegExp('xor', 'i')),

    comment: $ => token(seq('--', token.immediate(/[^\r\n]*(\r?\n)?/))),

    _header_comment: $ => seq(
      choice(
        // Header comment starts on the next line
        choice(token.immediate('\r\n'), token.immediate('\n')),
        // Or header comment starts on the current line
        alias($.comment, $.header_comment),
      ),
      repeat(alias($.comment, $.header_comment)),
      optional(choice('\r\n', '\n'))
    ),

    class_declaration: $ => seq(
      optional($.notes),
      optional(field('mark', $.header_mark)),
      keyword($, 'class'),
      $.class_name,
      optional($.formal_generics),
      optional($.obsolete),
      repeat($.inheritance),
      repeat($.creation_clause),
      optional($.converters),
      repeat($.feature_clause),
      optional($.invariant),
      optional($.notes),
      keyword($, 'end')
    ),

    converters: $ => seq(
      keyword($, 'convert'),
      $.converter,
      repeat(seq(',', $.converter))
    ),

    converter: $ => choice(
      $.conversion_procedure,
      $.conversion_query
    ),

    conversion_procedure: $ => seq(
      $.identifier,
      '(',
      '{',
      $._type_list,
      '}',
      ')'
    ),

    conversion_query: $ => seq(
      $.identifier,
      ':',
      '{',
      $._type_list,
      '}'
    ),

    _type_list: $ => seq(
      $._type,
      repeat(seq(',', $._type))
    ),

    creation_clause: $ => seq(
      keyword($, 'create'),
      optional($.clients),
      optional($._header_comment),
      join1($.identifier, ',')
    ),

    inheritance: $ => seq(
      keyword($, 'inherit'),
      optional($.non_conformance),
      repeat(';'),
      repeat(seq($.parent, repeat(';')))
    ),

    non_conformance: $ => seq(
      '{',
      keyword($, 'NONE'),
      '}'
    ),

    parent: $ => seq(
      $.class_type,
      optional($.feature_adaptation)
    ),

    // Just keyword($, 'end') is not valid, there should be at least one clause before it
    feature_adaptation: $ => seq(
      repeat1(choice(
        $.new_exports,
        $.undefine,
        $.redefine,
        $.rename,
        $.select,
      )),
      keyword($, 'end'),
    ),

    select: $ => seq(
      keyword($, 'select'),
      join1($.identifier, ',')
    ),

    new_exports: $ => seq(
      keyword($, 'export'),
      repeat(choice($.new_export_item, ';'))
    ),

    new_export_item: $ => seq(
      $.clients,
      optional($._header_comment),
      choice(
        keyword($, 'all'),
        join1($.identifier, ',')
      )
    ),

    rename: $ => seq(
      keyword($, 'rename'),
      $.rename_pair,
      repeat(seq(',', $.rename_pair))
    ),

    rename_pair: $ => seq(
      $.identifier,
      keyword($, 'as'),
      $.extended_feature_name
    ),

    extended_feature_name: $ => seq(
      $.identifier,
      repeat(seq(
        keyword($, 'alias'),
        $._manifest_string, // TODO: Alias_name should be here
        optional(keyword($, 'convert'))
      ))
    ),

    unary: $ => choice(
      $.unary_not,
      $.unary_plus_minus,
      $.free_operator
    ),

    unary_not: $ => keyword($, 'not'),

    unary_plus_minus: $ => prec(PREC.UNARY, choice('+', '-')),

    redefine: $ => seq(
      keyword($, 'redefine'),
      join1($.identifier, ',')
    ),

    undefine: $ => seq(
      keyword($, 'undefine'),
      join1($.identifier, ',')
    ),

    class_type: $ => seq(
      optional(choice(keyword($, 'expanded'), seq($.attachment_mark, optional($.separate_mark)), $.separate_mark)),
      $.class_name,
      optional($.actual_generics)
    ),

    separate_mark: $ => keyword($, 'separate'),

    attachment_mark: $ => choice(
      keyword($, 'attached'),
      keyword($, 'detachable')
    ),

    actual_generics: $ => seq(
      '[',
      $._type_list,
      ']'
    ),

    formal_generics: $ => seq(
      '[',
      $.formal_generic,
      repeat(seq(',', $.formal_generic)),
      ']'
    ),

    formal_generic: $ => seq(
      optional(choice(keyword($, 'frozen'), keyword($, 'expanded'), keyword($, 'reference'))),
      // Reference is contextual: a lone formal parameter may still use that name.
      choice($.class_name, alias($._keyword_reference, $.class_name)),
      optional($.constraint)
    ),

    constraint: $ => seq(
      '->',
      $.constraining_types,
      optional($.constraint_creators)
    ),

    constraining_types: $ => choice(
      $.single_constraint,
      $.multiple_constraint
    ),

    single_constraint: $ => seq(
      $._type,
      optional(seq($.rename, keyword($, 'end')))
    ),

    multiple_constraint: $ => seq(
      '{',
      $.single_constraint,
      repeat(seq(',', $.single_constraint)),
      '}'
    ),

    constraint_creators: $ => seq(
      keyword($, 'create'),
      join1($.identifier, ','),
      keyword($, 'end')
    ),

    invariant: $ => seq(
      keyword($, 'invariant'),
      repeat(';'),
      repeat($.assertion_clause)
    ),

    assertion_clause: $ => seq(
      optional($.tag_mark),
      choice(
        prec.right(2, $.expression),
        $.comment,
        $.object_free_assertion
      ),
      repeat(';')
    ),

    tag_mark: $ => seq(
      $.tag,
      ':'
    ),

    object_free_assertion: $ => keyword($, 'class'),

    feature_clause: $ => seq(
      keyword($, 'feature'),
      optional($.clients),
      optional($._header_comment),
      repeat(';'),
      field('declarations', repeat($.feature_declaration))
    ),

    clients: $ => seq(
      '{',
      optional(seq($.class_name, repeat(seq(',', $.class_name)))),
      '}'
    ),

    feature_declaration: $ => prec.dynamic(2, seq(
      join1($.new_feature, ','),

      choice(
        // Constant
        seq($._query_mark, $.explicit_value, optional($._header_comment)),

        // Field
        seq($._query_mark, optional($._header_comment)),

        // Routine
        seq(
          optional($.formal_arguments),
          optional($._query_mark),
          optional($._header_comment),
          optional($.obsolete),
          $.attribute_or_routine,
        )
      ),
      repeat(';')
    )),

    _query_mark: $ => seq(
      ':',
      $._type,
      optional($.assigner_mark)
    ),

    assigner_mark: $ => seq(
      keyword($, 'assign'),
      $.identifier
    ),

    explicit_value: $ => seq(
      '=',
      choice($._manifest_constant, keyword($, 'unique'))
    ),

    new_feature: $ => seq(
      optional(keyword($, 'frozen')),
      $.extended_feature_name,
      optional('\0')
    ),

    obsolete: $ => seq(
      keyword($, 'obsolete'),
      $._manifest_string
    ),

    attribute_or_routine: $ => seq(
      optional($.notes),
      optional($.precondition),
      optional($.local_declarations),
      $.feature_body,
      optional($.postcondition),
      optional($.rescue),
      keyword($, 'end')
    ),

    rescue: $ => seq(
      keyword($, 'rescue'),
      repeat($._instruction)
    ),

    local_declarations: $ => seq(
      keyword($, 'local'),
      repeat(';'),
      optional($._entity_declaration_list)
    ),

    precondition: $ => prec.left(seq(
      keyword($, 'require'),
      optional(keyword($, 'else')),
      repeat(';'),
      repeat($.assertion_clause)
    )),

    postcondition: $ => seq(
      keyword($, 'ensure'),
      optional(keyword($, 'then')),
      repeat(';'),
      repeat($.assertion_clause),
      optional($.only)
    ),

    only: $ => seq(
      keyword($, 'only'),
      join1($.identifier, ',')
    ),

    feature_body: $ => choice(
      keyword($, 'deferred'),
      $.effective_routine,
      $.attribute,
    ),

    attribute: $ => seq(
      keyword($, 'attribute'),
      repeat($._instruction)
    ),

    effective_routine: $ => choice(
      $.internal,
      $.external
    ),

    external: $ => seq(
      keyword($, 'external'),
      $._manifest_string, // TODO: External_language
      optional(
        seq(
          keyword($, 'alias'),
          $._manifest_string
        )
      )
    ),

    internal: $ => seq(
      $.routine_mark,
      repeat($._instruction)
    ),

    routine_mark: $ => choice(
      keyword($, 'do'),
      $.once
    ),

    once: $ => choice(
      keyword($, 'once'),
      prec.left(2, seq(keyword($, 'once'), '(', optional(join1($._manifest_string, ',')), ')'))
    ),

    _instruction: $ => choice(
      ';', // _instruction is hidden, so semicolons will not appear in the tree
      $.assignment,
      $.creation,
      $.call,
      $.conditional,
      $.multi_branch,
      $.loop,
      $.debug,
      $.precursor,
      $.check,
      $.retry,
      $.separate
    ),

    separate: $ => seq(
      keyword($, 'separate'),
      join1(seq($.expression, keyword($, 'as'), $.identifier), ','),
      choice(keyword($, 'do'), keyword($, 'then')),
      repeat($._instruction),
      keyword($, 'end')
    ),

    retry: $ => keyword($, 'retry'),

    check: $ => seq(
      keyword($, 'check'),
      repeat(';'),
      repeat($.assertion_clause),
      optional($.notes),
      optional(seq(keyword($, 'then'), repeat($._instruction))),
      keyword($, 'end')
    ),

    debug: $ => seq(
      prec.left(2, seq(
        keyword($, 'debug'),
        optional(seq('(', optional(join1($._manifest_string, ',')), ')')),
      )),
      repeat($._instruction),
      keyword($, 'end')
    ),

    loop: $ => choice(
      seq('⟳', $.identifier, ':', $.expression, '¦', repeat($._instruction), '⟲'),
      seq(
      optional($.iteration),
      optional($.initialization),
      optional($.invariant),
      optional($.exit_condition),
      $.loop_body,
      optional($.variant),
      keyword($, 'end')
    )),

    quantifier_loop: $ => choice(
      seq($.iteration, $.quantifier_loop_body, keyword($, 'end')),
      seq(
        choice('∀', '∃'),
        $.identifier,
        ':',
        $.expression,
        '¦',
        $.expression
      )
    ),

    quantifier_loop_body: $ => choice(
      seq(keyword($, 'all'), $.expression),
      seq(keyword($, 'some'), $.expression)
    ),

    iteration: $ => seq(
      keyword($, 'across'),
      $.expression,
      keyword($, 'as'),
      $.identifier
    ),

    initialization: $ => prec.left(2, seq(
      keyword($, 'from'),
      repeat($._instruction)
    )),

    exit_condition: $ => seq(
      keyword($, 'until'),
      $.expression
    ),

    loop_body: $ => seq(
      keyword($, 'loop'),
      repeat($._instruction)
    ),

    variant: $ => seq(
      keyword($, 'variant'),
      optional($.tag_mark),
      $.expression
    ),

    multi_branch: $ => seq(
      keyword($, 'inspect'),
      $.expression,
      repeat($.when_part),
      optional($.else_part),
      keyword($, 'end')
    ),

    multi_branch_expression: $ => seq(
      keyword($, 'inspect'),
      $.expression,
      repeat($.when_part_expression),
      optional($.else_part_expression),
      keyword($, 'end')
    ),

    when_part_expression: $ => seq(
      keyword($, 'when'),
      join1($.choice, ','),
      keyword($, 'then'),
      $.expression
    ),

    when_part: $ => seq(
      keyword($, 'when'),
      join1($.choice, ','),
      keyword($, 'then'),
      repeat($._instruction)
    ),

    choice: $ => choice(
      $._constant,
      $.constant_interval,
    ),

    _constant: $ => choice(
      $._manifest_constant,
      $.identifier,
      $.class_constant
    ),

    class_constant: $ => seq(
      $.manifest_type,
      '.',
      $.identifier
    ),

    constant_interval: $ => seq(
      $._constant,
      '..',
      $._constant
    ),

    conditional: $ => seq(
      keyword($, 'if'),
      $.then_part,
      repeat(seq(keyword($, 'elseif'), $.then_part)),
      optional($.else_part),
      keyword($, 'end')
    ),

    then_part: $ => seq(
      $.expression,
      keyword($, 'then'),
      repeat($._instruction)
    ),

    else_part: $ => seq(
      keyword($, 'else'),
      repeat($._instruction)
    ),

    else_part_expression: $ => seq(
      keyword($, 'else'),
      $.expression,
    ),

    conditional_expression: $ => seq(
      keyword($, 'if'),
      $.then_part_expression,
      repeat(seq(keyword($, 'elseif'), $.then_part_expression)),
      $.else_part_expression,
      keyword($, 'end')
    ),

    then_part_expression: $ => seq(
      $.expression,
      keyword($, 'then'),
      $.expression
    ),

    // Covers normal Assignment and Assigner_call
    assignment: $ => seq(
      choice($.call, $.result, $.bracket_expression),
      choice(
        ':=',
        '?=' // obsolete "Assignment attempt"
      ),
      $.expression
    ),

    call: $ => prec.left(2, seq(
      optional(seq(target($), '.')),
      $.unqualified_call
    )),

    creation: $ => seq(
      keyword($, 'create'),
      optional($.manifest_type),
      choice(
        prec.left(100, seq(choice($.result, $.identifier), '.', $.unqualified_call)),
        choice($.result, $.identifier),
      )
    ),

    unqualified_call: $=> choice(
      $.identifier, // without args
      prec.left(PREC.ACTUALS, seq($.identifier, $.actuals)) // with args
    ),

    actuals: $ => seq(
      '(',
      $.expression,
      repeat(seq(',', $.expression)),
      ')'
    ),

    result: $ => keyword($, 'Result'),

    expression: $ => choice(
      /// Basic_expression
      // Read_only
      $.current,

      // Variable
      $.result,

      $.call, // Call includes Identifier (possibility in Variable)
      $.precursor,
      $.equality,
      $.parenthesized,
      $.old,
      $.operator_expression,
      $.bracket_expression,
      $.creation_expression,
      $.conditional_expression,
      $.quantifier_loop,
      $.multi_branch_expression,

      /// Special_expression
      $._manifest_constant,
      $.void,
      $.manifest_tuple,
      $.agent,
      $.object_test,
      $.once_string,
      $.address
    ),

    creation_expression: $ => choice(
      seq(keyword($, 'create'), $.manifest_type),
      prec.left(2, seq(keyword($, 'create'), $.manifest_type, '.', $.unqualified_call))
    ),

    bracket_expression: $ => prec.left(2, seq(
      choice(
        target($),
        $.once_string,
        $._manifest_constant,
        $.manifest_tuple
      ),
      '[',
      join1($.expression, ','),
      ']'
    )),

    manifest_tuple: $ => seq(
      '[',
      $.expression,
      repeat(seq(',', $.expression)),
      ']'
    ),

    once_string: $ => seq(
      keyword($, 'once'),
      $._manifest_string
    ),

    // Priorities from http://www.gobosoft.com/eiffel/syntax/#Operator
    operator_expression: $ => choice(
      prec.left(200, seq($.unary, $.expression)), // unary precedes binary

      prec.left(100, seq($.expression, $.free_operator, $.expression)),
      prec.right(90, seq($.expression, $.binary_caret, $.expression)), // ^ is right-assoc.!
      prec.left(80, seq($.expression, $.binary_mul_div, $.expression)),
      prec.left(70, seq($.expression, $.binary_plus_minus, $.expression)),
      prec.left(60, seq($.expression, $.binary_comparison, $.expression)),
      prec.left(50, seq($.expression, $.binary_and, $.expression)),
      prec.left(40, seq($.expression, $.binary_or, $.expression)),
      prec.left(30, seq($.expression, $.binary_implies, $.expression))
    ),

    // Lookahead keeps comments and delimiter pairs outside the operator span.
    free_operator: $ => $._free_operator,

    binary_plus_minus: $ => choice('+', '-'),
    binary_mul_div: $ => choice('*', '/', '//', '\\\\'),
    binary_caret: $ => '^',
    binary_dot_dot: $ => choice('..'),
    binary_comparison: $ => choice('<', '<=', '>=', '>'),
    binary_and: $ => choice(keyword($, 'and'), 'and then'),
    binary_or: $ => choice(keyword($, 'or'), 'or else', keyword($, 'xor')),
    binary_implies: $ => choice(keyword($, 'implies'), '⇒'),

    old: $ => prec.left(2, seq(
      keyword($, 'old'),
      $.expression
    )),

    parenthesized: $ => seq(
      '(',
      $.expression,
      ')'
    ),

    equality: $ => prec.left(2, seq(
      $.expression,
      $.comparison,
      $.expression
    )),

    comparison: $ => choice('=', '/=', '~', '/~'),

    precursor: $ => choice(
      prec.left(PREC.ACTUALS, seq(keyword($, 'Precursor'), $.actuals)),
      prec.left(PREC.ACTUALS, seq(keyword($, 'Precursor'),
                                  seq('{', $.class_name, '}'),
                                  $.actuals)),
      seq(keyword($, 'Precursor'), seq('{', $.class_name, '}')),
      keyword($, 'Precursor')
    ),

    current: $ => keyword($, 'Current'),

    object_test: $ => prec.left(2, seq(
      keyword($, 'attached'),
      optional($.manifest_type),
      $.expression,
      optional(seq(keyword($, 'as'), $.identifier))
    )),

    address: $ => seq('$', choice($.identifier, $.current, $.result)),

    at_cursor: $ => seq('@', $.identifier),

    agent: $ => choice(
      $.call_agent,
      $.inline_agent
    ),

    inline_agent: $ => prec.left(2, seq(
      keyword($, 'agent'),
      optional($.formal_arguments),
      optional(seq(':', $._type)),
      $.attribute_or_routine,
      optional(
        // Agent_actuals
        seq(
          '(',
          optional(join1($._agent_actual, ',')),
          ')'
        )
      )
    )),

    call_agent: $ => choice(
      prec.left(2, seq(keyword($, 'agent'), $.agent_target, '.', $.agent_unqualified)),
      prec.left(2, seq(keyword($, 'agent'), $.agent_unqualified))
    ),

    agent_target: $ => prec(2, choice(
      $.identifier,
      $.parenthesized,
      $.manifest_type
    )),

    agent_unqualified: $ => choice(
      $.identifier,
      prec.left(2, seq(
        $.identifier,

        // Agent_actuals
        '(',
        optional(join1($._agent_actual, ',')),
        ')'
      ))
    ),

    _agent_actual: $ => choice($.expression, $.placeholder),

    placeholder: $ => seq(
      optional($.manifest_type),
      '?'
    ),

    manifest_array: $ => seq(
      '<<', optional(join1($.expression, ',')), '>>'
    ),

    void: $ => keyword($, 'Void'),

    formal_arguments: $ => seq(
      '(',
      repeat(';'),
      optional($._entity_declaration_list),
      ')'
    ),

    _entity_declaration_list: $ => repeat1(seq($.entity_declaration_group, repeat(';'))),

    entity_declaration_group: $ => seq(
      join1($.identifier, ','),
      ':',
      $._type
    ),

    _type: $ => choice(
      $.class_type,
      $.tuple_type,
      $.anchored,
    ),

    class_type: $ => seq(
      optional(choice(keyword($, 'expanded'), seq($.attachment_mark, optional($.separate_mark)), $.separate_mark)),
      $.class_name,
      optional($.actual_generics)
    ),

    tuple_type: $ => seq(
      optional($.attachment_mark),
      optional($.separate_mark),
      alias(keyword($, 'TUPLE'), $.class_name),
      optional(seq('[', optional($.tuple_parameter_list), ']')),
    ),

    tuple_parameter_list: $ => choice(
      $._type_list,
      seq(repeat(';'), $._entity_declaration_list),
      repeat1(';')
    ),

    anchored: $ => seq(
      optional($.attachment_mark),
      optional($.separate_mark),
      keyword($, 'like'),
      choice($.call, $.current)
    ),

    header_mark: $ => choice(
      seq(keyword($, 'frozen'), optional(choice(keyword($, 'deferred'), keyword($, 'expanded'), keyword($, 'once')))),
      keyword($, 'deferred'), keyword($, 'expanded'), keyword($, 'once')
    ),

    notes: $ => seq(keyword($, 'note'), repeat(';'), repeat(seq($.note_entry, repeat(';')))),

    note_entry: $ => seq(
      $.tag,
      ':',
      join1(choice($.identifier, $._manifest_constant), ',')
    ),

    _manifest_constant: $ => choice(
      $._manifest_value,
      prec.left(2, seq($.manifest_type, $._manifest_value))
    ),

    manifest_type: $ => seq('{', $._type, '}'),

    _manifest_value: $ => choice(
      $.boolean_constant,
      $.character_constant,
      $.integer_constant,
      $.real_constant,
      $._manifest_string,
      $.manifest_type,
      $.manifest_array
    ),

    _manifest_string: $ => choice(
      $.basic_manifest_string,
      $.verbatim_string,
    ),

    // TODO: Support {} brackets
    // TODO: Support tagged brackets ("named[ ... ]named")
    verbatim_string: $ => seq(
      $.verbatim_string_opener,
      alias(repeat(seq(
        EAT_SPACES,
        choice(
          seq(
            token.immediate(']'),
            choice(
              token.immediate(/[^"][^\n]*\r?\n/),
              token.immediate('\r'),
              token.immediate('\n')
            )
          ),
          token.immediate(/[^\]\r\n][^\n]*\r?\n/),
          token.immediate(/\r?\n/)
        )
      )), $.verbatim_string_content),
      $.verbatim_string_closer,
    ),

    verbatim_string_opener: $ => /"\[[ \t]*\r?\n/,

    verbatim_string_closer: $ => seq(EAT_SPACES, ']"'),

    basic_manifest_string: $ => seq(
      '"',
      optional($.string_content),
      '"'
    ),

    string_content: $ => repeat1(
      choice(
        alias(token(prec(1, /%./)), $.special_text), // escaped Special Character
        token(prec(1, /[^%"]+/)), // simple text span
      )
    ),

    real_constant: $ => seq(
      optional($.sign),
      $.real
    ),

    real: $ => seq(
      choice(
        prec.left(2, seq(alias($._decimal_integer, $.integer), token.immediate('.'), optional(alias(token.immediate(/[0-9]+(_+[0-9]+)*/), $.integer)))), // with integral
        seq('.', alias(token.immediate(/[0-9]+(_+[0-9]+)*/), $.integer)) // no integral, only fractional
      ),
      optional($.real_exponent)
    ),

    // Unsigned exponents compete with identifiers such as e6, so they stay one token.
    real_exponent: $ => choice(
      seq(
        token.immediate(choice('e', 'E')),
        alias(token.immediate(/[+-]/), $.sign),
        alias(token.immediate(/[0-9]+(_+[0-9]+)*/), $.integer)
      ),
      token.immediate(prec(1, /[eE][0-9]+(_+[0-9]+)*/))
    ),

    integer_constant: $ => seq(
      optional($.sign),
      $.integer
    ),

    sign: $ => choice('-', '+'),

    // Base prefixes belong to one token; real components remain decimal.
    integer: $ => choice(
      $._decimal_integer,
      /0[xX][0-9a-fA-F]+(_+[0-9a-fA-F]+)*/,
      /0[cC][0-7]+(_+[0-7]+)*/,
      /0[bB][01]+(_+[01]+)*/
    ),

    _decimal_integer: $ => /[0-9]+(_+[0-9]+)*/,

    character_constant: $ => seq("'", $.character, token.immediate("'")),

    // Whitespace inside a character literal is payload, never a grammar extra.
    character: $ => token.immediate(choice(
      /[^%'\r\n]/,
      /%[ABCDFHLNQRSTUV%'"()<>]/,
      seq('%/', choice(
        /[0-9]+(_+[0-9]+)*/,
        /0[xX][0-9a-fA-F]+(_+[0-9a-fA-F]+)*/,
        /0[cC][0-7]+(_+[0-7]+)*/,
        /0[bB][01]+(_+[01]+)*/
      ), '/')
    )),

    boolean_constant: $ => choice(keyword($, 'True'), keyword($, 'False')),

    class_name: $ => $._id,
    identifier: $ => $._id,
    tag: $ => $._id,

    _id: $ => /[a-zA-Z_][a-zA-Z0-9_]*/
  }
});
