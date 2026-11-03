/**
 * @file Tree sitter grammar for VB.NET
 * @author CodeAnt AI <chinmay@codeant.ai>
 * @license MIT
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

// Tree-sitter grammar for Visual Basic .NET, based on the official VB.NET language specification.
module.exports = grammar({
  name: 'vb_dotnet',

  // Keywords are extracted from this token, so `Domains` stays one identifier
  // instead of lexing as the keyword `Do` followed by `mains`.
  word: $ => $.identifier,


  extras: $ => [
    $.comment,
    /[ \t\f\u00A0\uFEFF]+/,  // whitespace except newlines, plus a stray BOM
    $._line_continuation    
  ],

  
  conflicts: $ => [
    [$.type, $.invocation],
    [$.type] ,
    [$.new_expression] ,
    [$.type_argument_list] ,
    [$.property_declaration] ,
    [$.constructor_declaration],
    [$.method_declaration],
    [$.left_hand_side, $.expression],
    [$.label_statement, $.expression],
    [$.event_declaration],
    [$.if_statement],
    [$.type, $.array_type],
    [$.if_statement, $.binary_expression],
    [$.empty_statement, $.if_statement],
    [$.class_block, $._member_declaration],
    [$.namespace_name, $.attribute],
    [$.namespace_name],
    [$.namespace_name, $.expression],
    [$.type, $.new_expression],
    [$.array_rank_specifier, $.argument_list],
    [$.inline_statement],
    [$.as_clause],
    [$.type_of_expression, $.binary_expression],
    [$.query_range, $.element_access],
    [$.query_range, $.binary_expression],
    [$.query_range, $.expression],
    [$.query_clause, $.expression],
    [$.query_clause],
  ],

  rules: {
    
    // `Option` and `Imports` lines are not required to lead the file: comments,
    // blank lines and any order between the two are all legal VB.NET.
    source_file: $ => repeat(choice(
      $.option_statements,
      $.imports_statement,
      $.attribute_block,
      $.namespace_block,
      $.type_declaration,
      $.preprocessor_directive,
      alias($._terminator, $.blank_line)
    )),

    // One `Option` line. The plural name is kept for query compatibility.
    option_statements: $ => seq(
      kw('Option'),
      choice(
        seq(kw('Explicit'), choice(kw('On'), kw('Off'))),
        seq(kw('Strict'), choice(kw('On'), kw('Off'))),
        seq(kw('Infer'), choice(kw('On'), kw('Off'))),
        seq(kw('Compare'), choice(kw('Binary'), kw('Text')))
      ),
      $._terminator
    ),

    // Imports statement for namespace imports (can import multiple namespaces in one line)
    imports_statement: $ => seq(
      kw('Imports'),
      commaSep1(seq(
        // Aliased import: `Imports NS = Some.Long.Namespace`
        optional(seq(field('alias', $.identifier), '=')),
        field('namespace', $.namespace_name)
      )),
      $._terminator
    ),

    // A dot-separated name (for namespaces or qualified types)
    namespace_name: $ => seq($.identifier, repeat(seq($._dot, $.identifier))),

    // Any top-level type or namespace declaration
    type_declaration: $ => choice(
      $.class_block,
      $.module_block,
      $.structure_block,
      $.interface_block,
      $.enum_block,
      $.delegate_declaration
    ),

    // Namespace block: Namespace Name ... End Namespace
    // namespace_block: $ => seq(
    //   kw('Namespace'),
    //   field('name', $.namespace_name),
    //   $._terminator,
    //   repeat(choice($.attribute_block, $.type_declaration, $.namespace_block)),
    //   kw('End'), kw('Namespace'), $._terminator
    // ),
    namespace_block: $ => prec.right(seq(
      kw('Namespace'),
      field('name', $.namespace_name),
      $._terminator,
      repeat(choice(
        $.attribute_block,
        $.type_declaration,
        $.namespace_block,
        $.imports_statement,  // Allow imports inside namespace
        $.preprocessor_directive,
        alias($._terminator, $.blank_line)  // Allow blank lines
      )),
      kw('End'), kw('Namespace'), optional($._terminator)
    )),

    // Class definition block
    class_block: $ => prec.right(seq(
      field('modifiers', optional($.modifiers)),
      kw('Class'),
      field('name', $.identifier),
      optional($.type_parameters),
      // `Inherits`/`Implements` may sit on the same line as the class name or
      // on their own lines — the canonical VB.NET layout is one per line, and
      // the header must keep consuming them before the member list starts.
      optional(seq(
        optional($._terminator),
        field('inherits', $.inherits_clause),
        optional($._terminator),
        optional(field('implements', $.implements_clause))
      )),
      optional(seq(
        optional($._terminator),
        field('implements', $.implements_clause)
      )),
      $._terminator,
      repeat($._member_declaration),
      kw('End'), kw('Class'), optional($._terminator)
    )),

    // Module definition block
    module_block: $ => prec.right(seq(
      field('modifiers', optional($.modifiers)),
      kw('Module'),
      field('name', $.identifier),
      $._terminator,
      repeat($._member_declaration),
      kw('End'), kw('Module'), optional($._terminator)
    )),

    // Structure definition block
    structure_block: $ => prec.right(seq(
      field('modifiers', optional($.modifiers)),
      kw('Structure'),
      field('name', $.identifier),
      optional($.type_parameters),
      optional(seq(
        optional($._terminator),
        field('implements', $.implements_clause)
      )),
      $._terminator,
      repeat($._member_declaration),
      kw('End'), kw('Structure'), optional($._terminator)
    )),

    // Interface definition block
    interface_block: $ => prec.right(seq(
      field('modifiers', optional($.modifiers)),
      kw('Interface'),
      field('name', $.identifier),
      optional($.type_parameters),
      optional(seq(
        optional($._terminator),
        field('inherits', $.inherits_clause) // interfaces can inherit multiple interfaces
      )),
      $._terminator,
      repeat($._member_declaration),
      kw('End'), kw('Interface'), optional($._terminator)
    )),

    // Enum definition block
    enum_block: $ => prec.right(seq(
      field('modifiers', optional($.modifiers)),
      kw('Enum'),
      field('name', $.identifier),
      optional($.as_clause),
      $._terminator,
      repeat($.enum_member),
      kw('End'), kw('Enum'), optional($._terminator)
    )),
    enum_member: $ => seq(
      field('name', $.identifier),
      optional(seq('=', field('value', $.expression))),
      $._terminator
    ),

    // Delegate declaration (treated as a type declaration, no End block since it's a single line)
    delegate_declaration: $ => seq(
      field('modifiers', optional($.modifiers)),
      kw('Delegate'),
      choice(kw('Sub'), kw('Function')),
      field('name', $.identifier),
      optional($.type_parameters),
      field('parameters', $.parameter_list),
      optional(seq(kw('As'), field('return_type', $.type))),  // only for Function delegate
      $._terminator
    ),

    // Inheritance clause (for classes or interfaces)
    inherits_clause: $ => seq(kw('Inherits'), commaSep1($.type)),
    // Implements clause (for classes or structures implementing interfaces)
    implements_clause: $ => seq(kw('Implements'), commaSep1($.type)),
    // Handles clause on a method: `Handles Button1.Click, MyBase.Load`
    handles_clause: $ => seq(kw('Handles'), commaSep1($.namespace_name)),

    // Generic type parameter definitions: e.g., (Of T As {Constraint})
    type_parameters: $ => seq(
      $._of_open,
      commaSep1($.type_parameter),
      ')'
    ),

    // `(Of` as one token: with a bare '(' the parser cannot tell a type argument
    // list from a constructor argument list until it has read a second token.
    _of_open: $ => token(seq('(', /[ 	]*/, /[Oo][Ff]/, /[ 	]/)),
    type_parameter: $ => seq(
      field('name', $.identifier),
      optional(seq(kw('As'), field('constraint', $.type_constraint)))
    ),
    type_constraint: $ => choice(
      // A constraint list: `As {IFoo, IBar, New}`
      seq('{', commaSep1($.type_constraint), '}'),
      $.type,
      kw('Structure'),
      kw('Class'),
      kw('New')  // type must have a public parameterless constructor
    ),

    // Attributes: <...> blocks attached to declarations
    attribute_block: $ => prec.right(seq(
      '<',
      optional($._newline),
      commaSep1($.attribute),
      optional($._newline),
      '>',
      // A trailing `_` line continuation eats the newline, so it may be absent.
      optional($._terminator)
    )),
    // attribute: $ => seq(
    //   optional(seq(field('target', $.identifier), ':')),  // e.g., Assembly: or Module: target
    //   field('name', $.identifier),
    //   optional($.argument_list)  // arguments in parentheses
    // ),
    attribute: $ => seq(
      optional(seq(field('target', $.identifier), ':')),
      field('name', choice($.identifier, $.namespace_name)),
      optional($.argument_list)
    ),

    // Modifiers (public/private/etc.). Multiple modifiers can appear in sequence.
    modifiers: $ => repeat1($.modifier),
    modifier: $ => token(choice(
      kw('Public'), kw('Private'), kw('Protected'), kw('Friend'),
      kw('Shared'), kw('Shadows'), kw('Static'),
      kw('Overloads'), kw('Overrides'), kw('Overridable'), kw('NotOverridable'), kw('MustOverride'),
      kw('MustInherit'), kw('NotInheritable'),
      kw('Partial'), kw('Narrowing'), kw('Widening'),
      kw('Default'),              // for default properties
      kw('ReadOnly'), kw('WriteOnly'),
      kw('WithEvents'), kw('Async'), kw('Iterator')
    )),

    // Type member declarations inside class/module/etc.
    _member_declaration: $ => choice(
      alias($._terminator, $.blank_line),
      $.const_declaration,
      $.field_declaration,
      $.method_declaration,
      $.constructor_declaration,
      $.property_declaration,
      $.event_declaration,
      $.operator_declaration,
      $.delegate_declaration,   // nested delegate type
      // Nested types. Listed one by one rather than through `type_declaration`,
      // which already carries `delegate_declaration` and would clash with it.
      $.class_block,
      $.module_block,
      $.structure_block,
      $.interface_block,
      $.enum_block,
      $.preprocessor_directive  // #Region / #If around members
    ),

    // Constant definitions (inside classes or procedures)
    const_declaration: $ => seq(
      optional(field('attributes', $.attribute_block)),
      field('modifiers', optional($.modifiers)),
      kw('Const'),
      commaSep1(seq(
        field('name', $.identifier),
        optional($.as_clause),
        '=', 
        field('value', $.expression)
      )),
      $._terminator
    ),

    // Field (variable) declarations (inside classes/modules or as locals with Dim)
    field_declaration: $ => seq(
      optional(field('attributes', $.attribute_block)),
      field('modifiers', optional($.modifiers)),
      optional(kw('Dim')),  
      commaSep1($.variable_declarator),
      $._terminator
    ),
    variable_declarator: $ => seq(
      field('name', $.identifier),
      optional(choice($.array_rank_specifier, $.array_bounds)),
      optional($.as_clause),
      optional(seq('=', optional($._newline), field('initializer', $.expression)))
    ),
    array_rank_specifier: $ => seq('(', optional(repeat(',')), ')'),
    // `Dim buffer(2047) As Char` — a rank specifier that carries bounds.
    array_bounds: $ => seq('(', commaSep1(seq($.expression, optional(seq(kw('To'), $.expression)))), ')'),  // e.g. "()" or "(,)" for array dimensions
    as_clause: $ => seq(
      kw('As'),
      choice(
        field('type', $.type),
        // `As New List(Of String)()` — object-initializer form, no `=`.
        seq(
          kw('New'),
          field('type', $.type),
          optional($.argument_list),
          optional($._creation_initializer)
        )
      )
    ),

    // Trailing initialiser on an object creation, in any of its three forms.
    _creation_initializer: $ => choice(
      $.object_initializers,
      $.with_initializer,
      // Collection initialiser: `New List(Of String) From {"a", "b"}`
      seq(kw('From'), $.object_initializers)
    ),

    // A type in a `New` expression, without a trailing array rank specifier.
    _created_type: $ => choice(
      $.primitive_type,
      $.generic_type,
      seq($.namespace_name, optional($.type_argument_list))
    ),

    // Type (for variables, parameters, return types, etc.)
    type: $ => seq(
      choice(
        $.primitive_type,
        $.array_type,
        $.generic_type,
        $.tuple_type,
        seq($.namespace_name, optional($.type_argument_list), optional($.array_rank_specifier))
      ),
      optional('?')  // nullable value type
    ),

    // Tuple type: `(index As Integer, page As Control)`
    tuple_type: $ => prec.right(seq(
      '(',
      $.tuple_element,
      repeat1(seq(comma(), $.tuple_element)),
      ')',
      optional($.array_rank_specifier)
    )),
    tuple_element: $ => seq(
      optional(field('name', $.identifier)),
      kw('As'),
      field('type', $.type)
    ),

    generic_type: $ => prec.left(3, seq(
      $.namespace_name,
      $.type_argument_list,
      // A generic name can carry further qualification, as in the interface
      // member form `IEqualityComparer(Of T).Equals`.
      repeat(seq($._dot, $.identifier, optional($.type_argument_list))),
      optional($.array_rank_specifier)
    )),

    array_type: $ => seq(
      choice($.primitive_type, $.namespace_name),
      optional($.type_argument_list),
      $.array_rank_specifier
    ),

    primitive_type: $ => token(choice(  // built-in types
      kw('Boolean'), kw('Byte'), kw('Short'), kw('Integer'), kw('Long'),
      kw('Single'), kw('Double'), kw('Decimal'),
      kw('Char'), kw('String'),
      kw('Object'), kw('Date')
    )),
    type_argument_list: $ => seq($._of_open, commaSep1($.type), ')'),

    // Method (Sub/Function) declaration inside a class/module or as a procedure in a module
    method_declaration: $ => seq(
      optional(field('attributes', $.attribute_block)),
      field('modifiers', optional($.modifiers)),
      choice(kw('Sub'), kw('Function')),
      field('name', $.identifier),
      optional($.type_parameters),
      field('parameters', $.parameter_list),
      optional(seq(kw('As'), field('return_type', $.type))),  // only for Function
      optional($.implements_clause),
      optional($.handles_clause),
      choice(
        // With body:
        seq($._terminator, repeat($.statement), kw('End'), choice(kw('Sub'), kw('Function')), $._terminator),
        // Without body (e.g., abstract method in MustInherit class, or interface method):
        $._terminator
      )
    ),

    // Constructor (Sub New) declaration
    constructor_declaration: $ => seq(
      optional(field('attributes', $.attribute_block)),
      field('modifiers', optional($.modifiers)),
      kw('Sub'), kw('New'),
      field('parameters', $.parameter_list),
      choice(
        seq($._terminator, repeat($.statement), kw('End'), kw('Sub'), $._terminator),
        $._terminator
      )
    ),

    // Property declaration (auto or with getters/setters)
    property_declaration: $ => seq(
      optional(field('attributes', $.attribute_block)),
      field('modifiers', optional($.modifiers)),
      kw('Property'),
      field('name', $.identifier),
      optional(field('parameters', $.parameter_list)),  // indexed properties
      optional($.as_clause),
      optional($.implements_clause),
      choice(
        // Auto-property or declaration without body:
        seq(
          optional(seq('=', optional($._newline), field('initializer', $.expression))),
          optional($.implements_clause),
          $._terminator
        ),
        // Property with Get/Set accessors:
        seq(
          $._terminator,
          optional($.get_accessor),
          optional($.set_accessor),
          kw('End'), kw('Property'), $._terminator
        )
      )
    ),
    get_accessor: $ => seq(
      optional(field('modifiers', optional($.modifiers))),
      kw('Get'), $._terminator,
      repeat($.statement),
      kw('End'), kw('Get'), $._terminator
    ),
    set_accessor: $ => seq(
      optional(field('modifiers', optional($.modifiers))),
      kw('Set'),
      optional(field('parameters', $.parameter_list)),  // Set can have a Value parameter
      $._terminator,
      repeat($.statement),
      kw('End'), kw('Set'), $._terminator
    ),

    // Event declaration (regular or custom event with add/remove/raise handlers)
    event_declaration: $ => seq(
      optional(field('attributes', $.attribute_block)),
      field('modifiers', optional($.modifiers)),
      optional(kw('Custom')),
      kw('Event'),
      field('name', $.identifier),
      choice(
        // Standard event declaration
        seq(
          optional($.parameter_list),
          optional($.as_clause),
          optional($.implements_clause),
          $._terminator
        ),
        // Custom event with handlers
        seq(
          optional($.parameter_list), optional($.as_clause), $._terminator,
          repeat(choice($.add_handler_block, $.remove_handler_block, $.raise_event_block)),
          kw('End'), kw('Event'), $._terminator
        )
      )
    ),
    add_handler_block: $ => seq(
      optional(field('modifiers', optional($.modifiers))),
      kw('AddHandler'), optional(field('parameters', $.parameter_list)), $._terminator,
      repeat($.statement),
      kw('End'), kw('AddHandler'), $._terminator
    ),
    remove_handler_block: $ => seq(
      optional(field('modifiers', optional($.modifiers))),
      kw('RemoveHandler'), optional(field('parameters', $.parameter_list)), $._terminator,
      repeat($.statement),
      kw('End'), kw('RemoveHandler'), $._terminator
    ),
    raise_event_block: $ => seq(
      optional(field('modifiers', optional($.modifiers))),
      kw('RaiseEvent'), optional(field('parameters', $.parameter_list)), $._terminator,
      repeat($.statement),
      kw('End'), kw('RaiseEvent'), $._terminator
    ),

    // Operator overloads: `Public Shared Operator +(a As T, b As T) As T`
    operator_declaration: $ => seq(
      optional(field('attributes', $.attribute_block)),
      field('modifiers', optional($.modifiers)),
      kw('Operator'),
      field('name', choice(
        '+', '-', '*', '/', '^', '&', '<<', '>>',
        '=', '<>', '<', '>', '<=', '>=',
        kw('Not'), kw('And'), kw('Or'), kw('Xor'), kw('Mod'),
        kw('Like'), kw('IsTrue'), kw('IsFalse'), kw('CType')
      )),
      field('parameters', $.parameter_list),
      optional(seq(kw('As'), field('return_type', $.type))),
      $._terminator,
      repeat($.statement),
      kw('End'), kw('Operator'), $._terminator
    ),

    // Parameter list for methods/delegates (Parentheses with comma-separated parameters)
    parameter_list: $ => seq('(', optional($._newline), optional(commaSep($.parameter)), optional($._newline), ')'),
    // parameter: $ => seq(
    //   optional(choice(kw('ByVal'), kw('ByRef'), kw('ParamArray'))),
    //   field('name', $.identifier),
    //   optional($.array_rank_specifier),
    //   optional($.as_clause),
    //   optional(seq('=', field('default_value', $.expression)))  // default parameter value
    // ),
    parameter: $ => seq(
      optional($.attribute_block),  // Add support for attributes on parameters
      optional(kw('Optional')),
      optional(choice(kw('ByVal'), kw('ByRef'), kw('ParamArray'))),
      field('name', $.identifier),
      optional($.array_rank_specifier),
      optional($.as_clause),
      optional(seq('=', field('default_value', $.expression)))
    ),

    // *** Statements (inside procedures or blocks) ***

    statement: $ => choice(
      $.empty_statement,
      $.label_statement,
      $.dim_statement,
      $.const_declaration,      // local constant
      prec(1, $.assignment_statement),
      $.call_statement,
      $.if_statement,
      $.select_case_statement,
      $.while_statement,
      $.do_statement,
      $.for_statement,
      $.for_each_statement,
      $.try_statement,
      $.with_statement,
      $.using_statement,
      $.sync_lock_statement,
      $.return_statement,
      $.exit_statement,
      $.continue_statement,
      $.throw_statement,
      $.goto_statement,
      $.redim_statement,
      $.add_handler_statement,
      $.remove_handler_statement,
      $.raise_event_statement,
      $.erase_statement,
      $.stop_statement,
      $.on_error_statement,
      $.preprocessor_directive,  // allow preprocessor directives in code flow
      $.compound_assignment_statement,
    ),

    empty_statement: $ => prec(1, $._terminator),  // a standalone line break (no-op statement)

    label_statement: $ => seq(field('label', $.identifier), ':'),

    dim_statement: $ => seq(
      kw('Dim'),
      commaSep1(seq(
        field('name', $.identifier),
        optional('?'),
        optional(choice($.array_rank_specifier, $.array_bounds)),
        optional($.as_clause),
        optional(seq('=', optional($._newline), field('initializer', $.expression)))
      )),
      $._terminator
    ),

    compound_assignment_statement: $ => seq(
      field('left', $.left_hand_side),
      field('operator', choice('+=', '-=', '*=', '/=', '\\=', '^=', '&=', '<<=', '>>=')),
      field('right', $.expression),
      $._terminator
    ),

    assignment_statement: $ => seq(
      field('left', $.left_hand_side),
      '=',
      optional($._newline),
      field('right', $.expression),
      $._terminator
    ),
    left_hand_side: $ => choice(
      $.identifier,
      $.member_access,
      $.with_member_access,
      $.element_access,
      // `dic(key) += 1` — indexing an object reads as an invocation here.
      $.invocation
    ),

    call_statement: $ => seq(
      choice(seq(kw('Call'), $.expression), $.expression),
      $._terminator
    ),

    // if_statement: $ => choice(
    //   // Single-line If
    //   seq(
    //     kw('If'), field('condition', $.expression), kw('Then'),
    //     field('then_branch', $.statement),
    //     optional(seq(kw('Else'), field('else_branch', $.statement)))
    //   ),
    //   // Block If/ElseIf/Else
    //   seq(
    //     kw('If'), field('condition', $.expression), kw('Then'), $._terminator,
    //     repeat($.statement),
    //     repeat(seq(kw('ElseIf'), $.expression, kw('Then'), $._terminator, repeat($.statement))),
    //     optional(seq(kw('Else'), $._terminator, repeat($.statement))),
    //     kw('End'), kw('If'), $._terminator
    //   )
    // ),
    if_statement: $ => choice(
      // Single-line If
      prec(2, seq(
        kw('If'), field('condition', $.expression), kw('Then'),
        field('then_branch', $.inline_statements),
        optional(seq(kw('Else'), field('else_branch', $.inline_statements)))
      )),
      // Block If/ElseIf/Else
      prec(1, seq(
        kw('If'), field('condition', $.expression), kw('Then'), $._terminator,
        repeat($.statement),
        repeat($.elseif_clause),
        optional($.else_clause),
        kw('End'), kw('If'), $._terminator
      ))
    ),

    // Colon-separated statements on one line, with no terminator of their own.
    inline_statements: $ => prec.right(seq(
      $.inline_statement,
      repeat(seq(':', $.inline_statement))
    )),
    inline_statement: $ => prec.right(choice(
      seq(field('left', $.left_hand_side), '=', field('right', $.expression)),
      seq(
        field('left', $.left_hand_side),
        field('operator', choice('+=', '-=', '*=', '/=', '\=', '^=', '&=', '<<=', '>>=')),
        field('right', $.expression)
      ),
      seq(optional(kw('Call')), $.expression),
      seq(kw('Return'), optional($.expression)),
      seq(kw('Exit'), choice(kw('Sub'), kw('Function'), kw('Property'), kw('Do'), kw('For'), kw('While'), kw('Select'), kw('Try'))),
      seq(kw('Continue'), choice(kw('Do'), kw('For'), kw('While'))),
      seq(kw('Throw'), optional($.expression)),
      seq(kw('GoTo'), field('label', $.identifier)),
      seq(kw('AddHandler'), $.expression, comma(), $.expression),
      seq(kw('RemoveHandler'), $.expression, comma(), $.expression),
      seq(kw('RaiseEvent'), $.identifier, optional($.argument_list)),
      kw('Stop'),
      seq(kw('Dim'),
        field('name', $.identifier),
        optional(choice($.array_rank_specifier, $.array_bounds)),
        optional($.as_clause),
        optional(seq('=', field('initializer', $.expression)))
      )
    )),

    elseif_clause: $ => seq(
      kw('ElseIf'), field('condition', $.expression), kw('Then'), $._terminator,
      repeat($.statement)
    ),

    else_clause: $ => seq(
      kw('Else'), $._terminator,
      repeat($.statement)
    ),

    select_case_statement: $ => seq(
      kw('Select'), kw('Case'), field('selector', $.expression), $._terminator,
      repeat($.case_block),
      optional($.case_else_block),
      kw('End'), kw('Select'), $._terminator
    ),
    case_block: $ => seq(
      kw('Case'),
      commaSep1($.case_clause),
      $._terminator,
      repeat($.statement)
    ),
    case_else_block: $ => seq(
      kw('Case'), kw('Else'), $._terminator,
      repeat($.statement)
    ),
    case_clause: $ => choice(
      $.expression,
      seq($.expression, kw('To'), $.expression),                // range: low To high
      seq(optional(kw('Is')), $.relational_operator, $.expression)  // relational form: e.g. `Is < 5`, `> 0`
    ),
    relational_operator: $ => token(choice('=', '<>', '<', '>', '<=', '>=')),

    while_statement: $ => seq(
      kw('While'), field('condition', $.expression), $._terminator,
      repeat($.statement),
      kw('End'), kw('While'), $._terminator
    ),

    do_statement: $ => choice(
      // Do ... Loop [While/Until condition] (exit condition at bottom)
      seq(kw('Do'), $._terminator,
          repeat($.statement),
          kw('Loop'), optional(choice(seq(kw('While'), $.expression), seq(kw('Until'), $.expression))), $._terminator),
      // Do [While/Until condition] ... Loop (check at top)
      seq(kw('Do'), choice(seq(kw('While'), $.expression), seq(kw('Until'), $.expression)), $._terminator,
          repeat($.statement),
          kw('Loop'), $._terminator)
    ),

    // for_statement: $ => seq(
    //   kw('For'),
    //   field('variable', $.identifier), '=', field('start', $.expression),
    //   kw('To'), field('end', $.expression),
    //   optional(seq(kw('Step'), field('step', $.expression))),
    //   $._terminator,
    //   repeat($.statement),
    //   kw('Next'), optional(alias($.identifier, $.variable)), $._terminator
    // ),
    for_statement: $ => seq(
      kw('For'),
      choice(
        // With type declaration
        seq(
          field('variable', $.identifier),
          $.as_clause,
          '=',
          field('start', $.expression)
        ),
        // Without type declaration
        seq(
          field('variable', $.identifier),
          '=',
          field('start', $.expression)
        )
      ),
      kw('To'), field('end', $.expression),
      optional(seq(kw('Step'), field('step', $.expression))),
      $._terminator,
      repeat($.statement),
      // kw('Next'), optional(field('variable', $.identifier)), $._terminator
      kw('Next'), optional(field('variable', $.identifier)), $._terminator
    ),

    // for_each_statement: $ => seq(
    //   kw('For'), kw('Each'),
    //   field('variable', $.identifier),
    //   kw('In'),
    //   field('collection', $.expression),
    //   $._terminator,
    //   repeat($.statement),
    //   kw('Next'), optional(alias($.identifier, $.variable)), $._terminator
    // ),
    for_each_statement: $ => seq(
      kw('For'), kw('Each'),
      field('variable', $.identifier),
      optional($.as_clause),
      kw('In'),
      field('collection', $.expression),
      $._terminator,
      repeat($.statement),
      kw('Next'), optional(field('variable', $.identifier)), $._terminator
    ),

    try_statement: $ => seq(
      kw('Try'), $._terminator,
      repeat($.statement),
      repeat($.catch_block),
      optional($.finally_block),
      kw('End'), kw('Try'), $._terminator
    ),
    catch_block: $ => seq(
      kw('Catch'),
      optional(seq(field('exception', $.identifier), optional(seq(kw('As'), field('type', $.type))))),
      optional(seq(kw('When'), field('filter', $.expression))),
      $._terminator,
      repeat($.statement)
    ),
    finally_block: $ => seq(
      kw('Finally'), $._terminator,
      repeat($.statement)
    ),

    with_statement: $ => seq(
      kw('With'), field('target', $.expression), $._terminator,
      repeat($.statement),
      kw('End'), kw('With'), $._terminator
    ),

    using_statement: $ => seq(
      kw('Using'),
      choice(
        // Using resourceVar As Type = expr
        seq(
          field('resource', $.identifier),
          $.as_clause,
          optional(seq('=', field('value', $.expression)))
        ),
        // Using <expression>
        field('value', $.expression)
      ),
      $._terminator,
      repeat($.statement),
      kw('End'), kw('Using'), $._terminator
    ),

    sync_lock_statement: $ => seq(
      kw('SyncLock'), field('lock', $.expression), $._terminator,
      repeat($.statement),
      kw('End'), kw('SyncLock'), $._terminator
    ),

    return_statement: $ => seq(kw('Return'), optional($.expression), $._terminator),

    exit_statement: $ => seq(kw('Exit'), choice(kw('Sub'), kw('Function'), kw('Property'), kw('Do'), kw('For'), kw('While'), kw('Select'), kw('Try')), $._terminator),

    continue_statement: $ => seq(kw('Continue'), choice(kw('Do'), kw('For'), kw('While')), $._terminator),

    throw_statement: $ => seq(kw('Throw'), optional($.expression), $._terminator),

    goto_statement: $ => seq(kw('GoTo'), field('label', $.identifier), $._terminator),

    redim_statement: $ => seq(
      kw('ReDim'),
      optional(kw('Preserve')),
      commaSep1(seq(field('array', $.identifier), $.re_dim_clause)),
      $._terminator
    ),
    re_dim_clause: $ => seq(
      '(', field('upper_bound', $.expression), optional(seq(kw('To'), $.expression)), ')'
    ),

    add_handler_statement: $ => seq(
      kw('AddHandler'), field('event', $.expression), comma(), field('handler', $.expression), $._terminator
    ),
    remove_handler_statement: $ => seq(
      kw('RemoveHandler'), field('event', $.expression), comma(), field('handler', $.expression), $._terminator
    ),
    raise_event_statement: $ => seq(
      kw('RaiseEvent'), field('name', $.identifier), optional($.argument_list), $._terminator
    ),
    erase_statement: $ => seq(kw('Erase'), commaSep1($.expression), $._terminator),
    stop_statement: $ => seq(kw('Stop'), $._terminator),
    // `On Error GoTo 0` / `On Error Resume Next`
    on_error_statement: $ => seq(
      kw('On'), kw('Error'),
      choice(
        seq(kw('GoTo'), choice($.identifier, $.integer_literal)),
        seq(kw('Resume'), kw('Next'))
      ),
      $._terminator
    ),

    // Preprocessor directive (treated as a standalone statement)
    preprocessor_directive: $ => token(seq('#', /[^\r\n]*/)),

    // *** Expressions ***

    expression: $ => choice(
      $.literal,
      $.identifier,
      $.parenthesized_expression,
      $.member_access,
      $.element_access,
      $.invocation,
      $.unary_expression,
      $.binary_expression,
      $.ternary_expression,
      $.new_expression,
      $.lambda_expression,
      $.array_literal,
      $.type_of_expression,
      $.await_expression,
      $.with_member_access,
      $.query_expression,
    ),

    // LINQ query expressions. Clauses routinely sit on their own lines, so a
    // line break is allowed before each of them.
    query_expression: $ => prec.right(seq(
      kw('From'), $.query_range,
      repeat($.query_clause)
    )),
    query_range: $ => seq(
      field('name', $.identifier),
      optional($.as_clause),
      kw('In'),
      field('source', $.expression)
    ),
    // Each clause keyword may carry the line break that precedes it, so a query
    // can be laid out over several lines without ending the statement.
    query_clause: $ => prec.right(choice(
      seq(qkw('From'), $.query_range),
      seq(qkw('Where'), $.expression),
      seq(qkw('Let'), field('name', $.identifier), '=', $.expression),
      seq(qkw('Select'), optional(seq(field('name', $.identifier), '=')), $.expression),
      seq(qkw('Order'), kw('By'), $.expression, optional(choice(kw('Ascending'), kw('Descending')))),
      seq(qkw('Group'), kw('By'), $.expression, kw('Into'), optional(seq(field('name', $.identifier), '=')), $.expression),
      seq(qkw('Take'), optional(kw('While')), $.expression),
      seq(qkw('Skip'), optional(kw('While')), $.expression),
      qkw('Distinct')
    )),

    // `Await Task.Delay(1)`
    await_expression: $ => prec.right(9, seq(kw('Await'), field('operand', $.expression))),

    // Leading-dot member access inside a `With` block: `.Name = "x"`
    with_member_access: $ => prec.left(1, seq(
      '.',
      field('member', $.identifier)
    )),

    // TypeOf <expr> Is|IsNot <type>
    // The operand is deliberately narrow: with a full expression it would
    // swallow the `Is` as a binary operator.
    type_of_expression: $ => prec(9, seq(
      kw('TypeOf'),
      field('operand', choice(
        $.identifier,
        $.member_access,
        $.with_member_access,
        $.element_access,
        $.invocation,
        $.parenthesized_expression
      )),
      choice(kw('Is'), kw('IsNot')),
      field('type', $.type)
    )),

    array_literal: $ => seq(
      '{',
      optional($._newline),
      optional(commaSep($.expression)),
      optional($._newline),
      '}'
    ),

    parenthesized_expression: $ => seq('(', optional($._newline), $.expression, optional($._newline), ')'),

    lambda_expression: $ => prec.right(seq(
      optional(choice(kw('Async'), kw('Iterator'))),
      choice(kw('Function'), kw('Sub')),
      '(',
      optional(commaSep($.lambda_parameter)),
      ')',
      optional(seq(kw('As'), field('return_type', $.type))),
      choice(
        $.expression,  // Single expression lambda
        $.if_statement,  // single-line `Sub(x) If ... Then ...`
        seq(           // Multi-line lambda
          $._terminator,
          repeat($.statement),
          kw('End'), choice(kw('Function'), kw('Sub'))
        )
      )
    )),

    lambda_parameter: $ => seq(
      optional(choice(kw('ByVal'), kw('ByRef'))),
      field('name', $.identifier),
      optional($.as_clause)
    ),
    
    invocation: $ => prec.left(1, choice(
      seq(
        field('target', choice($.member_access, $.with_member_access, $.identifier)),
        field('arguments', $.argument_list)
      ),
      seq(
        field('target', choice($.member_access, $.with_member_access, $.identifier)),
        field('type_arguments', $.type_argument_list),
        optional(field('arguments', $.argument_list))
      )
    )),

    argument_list: $ => seq(
      '(',
      optional($._newline),
      optional(seq(
        optional($.argument),
        repeat(seq(comma(), optional($.argument)))
      )),
      optional($._newline),
      ')'
    ),
    argument: $ => choice(
      $.expression,
      seq(field('name', $.identifier), ':', '=', $.expression)  // named argument (Name:=Expr)
    ),

    // Member access (object.member) possibly spanning lines after the dot
    // member_access: $ => seq(
    //   field('object', $.expression),
    //   token(seq('.', optional(/\r?\n/))),  // allow newline after dot (implicit line continuation)
    //   field('member', $.identifier)
    // ),
    member_access: $ => prec.left(10, seq(
      field('object', $.expression),
      choice($._dot, $._null_dot),
      // `New` is a keyword but a legal member name (`MyBase.New`).
      field('member', choice($.identifier, alias(kw('New'), $.identifier)))
    )),

    // Element/array index access: expr(index, index, ...)
    element_access: $ => seq(
      field('object', $.expression),
      '(',
      optional($._newline),
      commaSep(field('index', $.expression)),
      optional($._newline),
      ')'
    ),

    // Object creation: New Type[(args)] [ with initializers ]
    // new_expression: $ => seq(
    //   kw('New'),
    //   field('type', $.type),
    //   optional($.argument_list),
    //   optional($.object_initializers)
    // ),
    // Binds tighter than member/element access so that `New X(a, b)` keeps its
    // argument list instead of reading it as an index on a bare `New X`.
    new_expression: $ => prec(11, seq(
      kw('New'),
      // The created type must not swallow `(...)` as an array rank specifier:
      // in `New Point(0, 0)` the parentheses are the constructor arguments.
      field('type', alias($._created_type, $.type)),
      optional($.argument_list),
      optional($._creation_initializer)
    )),

    with_initializer: $ => seq(
      kw('With'),
      '{',
      optional($._newline),
      optional(commaSep($.member_initializer)),
      optional($._newline),
      '}'
    ),

    member_initializer: $ => seq(
      '.',
      field('member', $.identifier),
      '=',
      field('value', $.expression)
    ),
    object_initializers: $ => seq(
      '{',
      optional($._newline),
      optional(commaSep($.object_initializer)),
      optional($._newline),
      '}'
    ),
    object_initializer: $ => choice(
      seq('.', $.identifier, '=', $.expression),  // assignment to member
      $.expression                               // value for collection initializer
    ),

    // Unary operators: e.g. -x, +x, Not x, AddressOf x
    unary_expression: $ => prec(8, seq(
      field('operator', choice(kw('Not'), kw('AddressOf'), '-', '+')),
      field('operand', $.expression)
    )),

    // Binary operators with precedence (higher number = higher precedence binding)
    binary_expression: $ => {
      const table = [
        [7, choice('^')],                              // exponentiation (right-associative in VB)
        [6, choice('*', '/', '\\', kw('Mod'))],        // multiplication, division, integer division, modulo
        [5, choice('+', '-')],                         // addition and subtraction
        [4, kw('&')],                                  // string concatenation
        [3, choice('<<', '>>')],                       // bit shifts
        [2, choice('=', '<>', '<', '>', '<=', '>=', kw('Is'), kw('IsNot'), kw('Like'))],  
        [0, choice(kw('And'), kw('Or'), kw('Xor'))],   // boolean/bitwise AND/OR/XOR
        [-1, choice(kw('AndAlso'), kw('OrElse'))]      // short-circuit logical operators (lowest precedence)
      ];
      // Build binary expression rules for each operator with correct precedence and associativity
      return choice(...table.map(([precedence, operator]) =>
        prec.left(precedence, seq(field('left', $.expression), field('operator', operator), optional($._newline), field('right', $.expression)))
      ));
    },

    // Ternary conditional (IIf-like or If operator: If(condition, trueExpr, falseExpr))
    ternary_expression: $ => prec.right(seq(
      kw('If'),
      '(',
      field('condition', $.expression), comma(),
      field('true_branch', $.expression),
      // Two-argument form: `If(value, fallback)`.
      optional(seq(comma(), field('false_branch', $.expression))),
      ')'
    )),

    // Literals
    literal: $ => choice(
      $.boolean_literal,
      $.integer_literal,
      $.floating_point_literal,
      $.string_literal,
      $.character_literal,
      $.date_literal,
      kw('Nothing')
    ),

    boolean_literal: $ => token(choice(kw('True'), kw('False'))),

    integer_literal: $ => token(choice(
      // Decimal literal (optional type suffix)
      /\d+(?:US|UI|UL|S|I|L|%|&)?/i,
      // Hexadecimal literal (prefix &H)
      /&H[0-9A-F]+(?:US|UI|UL|S|I|L|%|&)?/i,
      // Octal literal (prefix &O)
      /&O[0-7]+(?:US|UI|UL|S|I|L|%|&)?/i
    )),  

    floating_point_literal: $ => token(choice(
      // Formats: D (integer) . D (fraction) E? exponent, etc., with optional FP type suffix (F, R, D, !, #, @)
      /\d+\.\d+([Ee][+-]?\d+)?[FfRrDd!#@]?/,
      /\d+\.([Ee][+-]?\d+)?[FfRrDd!#@]?/,
      /\.\d+([Ee][+-]?\d+)?[FfRrDd!#@]?/,
      /\d+([Ee][+-]?\d+)[FfRrDd!#@]?/,
      /\d+[FfRrDd!#@]/
    )),

    // string_literal: $ => token(seq(
    //   '"',
    //   repeat(choice(/[^"\r\n]/, /""/)),  // double "" inside represents a quote
    //   '"'
    // )),
    string_literal: $ => choice(
      // Regular string
      token(seq(
        '"',
        repeat(choice(/[^"\r\n]/, /""/)),
        '"'
      )),
      // Interpolated string
      $.interpolated_string_literal
    ),

    interpolated_string_literal: $ => seq(
      '$"',
      repeat(choice(
        // The precedence keeps an apostrophe inside the text from starting a
        // comment that would swallow the rest of the line.
        token(prec(2, /[^"{}\r\n]+/)),
        /""/,
        $.interpolation
      )),
      '"'
    ),

    interpolation: $ => seq(
      '{',
      $.expression,
      optional(seq(':', /[^}]+/)),  // format specifier
      '}'
    ),

    character_literal: $ => token(seq(
      '"',
      choice(/[^"\r\n]/, /""/),  // exactly one character (or an escaped quote)
      '"',
      /[cC]/                    // 'C' suffix (case-insensitive)
    )),

    // Date literal: #MM/dd/yyyy [hh:mm[:ss] [AM|PM]]#
    date_literal: $ => token(seq(
      '#',
      /[0-9/\-:\sAPMapm]+/,  // simplified pattern: digits, '/', '-', ':', whitespace, AM/PM
      '#'
    )),

    // Identifiers (case-insensitive, including escaped [bracketed] identifiers and type-char suffix)
    identifier: $ => token(choice(
      // Escaped identifier in [] (can be a keyword inside)
      /\[(?:[^\]\r\n]+)\]/,
      // Unescaped identifier. VB.NET uses the same Unicode identifier classes
      // as C#, so non-ASCII names are legal. The trailing character is the
      // optional type-declaration suffix (e.g. foo$, bar!).
      /[\p{L}\p{Nl}_][\p{L}\p{Nl}\p{Mn}\p{Mc}\p{Nd}\p{Pc}\p{Cf}]*[$%&@#!]?/
    )),

    // Comment: `'` or `REM` to end of line. `REM` must be followed by a
    // separator, or it would eat identifiers such as `RemoveAll`.
    comment: $ => token(choice(
      seq("'", /[^\r\n]*/),
      seq(/[Rr][Ee][Mm]/, /[ \t:][^\r\n]*/)
    )),

    // Line break (statement terminator)
    _newline: $ => /\r?\n/,

    _line_continuation: $ => token(seq('_', /[ \t]*/, /\r?\n/)),

    // A member-access dot may end its line: `list.` then `Where(...)` below.
    // The precedence keeps a dot after an expression attached to it, rather
    // than starting a `With`-block member access.
    _dot: $ => token(prec(1, seq('.', optional(/\r?\n/)))),
    _null_dot: $ => token(prec(1, seq('?.', optional(/\r?\n/)))),

    // Statement terminator: newline or colon (for multiple statements on one line)
    _terminator: $ => choice($._newline, ':')
  }
});

// Helper: comma-separated list (zero or more)
function commaSep(rule) {
  return optional(commaSep1(rule));
}

// A query-clause keyword, which may take the line break in front of it.
function qkw(word) {
  return token(seq(optional(/\r?\n[ \t]*/), ci(word)));
}

function kw(word) {
  // Plain token: the grammar's `word` rule drives keyword extraction, so a
  // keyword only wins when it spans a whole identifier.
  return token(ci(word));
}

// Helper: comma-separated list (one or more), allowing a newline after commas
function commaSep1(rule) {
  return seq(rule, repeat(seq(comma(), rule)));
}

// A comma that may be followed by a line break (implicit line continuation).
function comma() {
  return token(seq(',', optional(/\r?\n/)));
}

// Case-insensitive literal helper: creates a regex that matches the given keyword in any case
function ci(keyword) {
  const pattern = keyword.split('').map(ch => {
    if (/[A-Za-z]/.test(ch)) {
      return `[${ch.toLowerCase()}${ch.toUpperCase()}]`;
    }
    // Escape any regex special characters (though keywords are alphabetic in VB)
    return ch.replace(/[-/\\^$*+?.()|[\]{}]/g, '\\$&');
  }).join('');
  return new RegExp(pattern);
}

