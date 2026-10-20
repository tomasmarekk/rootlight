/**
 * @file Wat grammar for tree-sitter
 * @author Pig Fang <g-plane@hotmail.com>
 * @license MIT
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

export default grammar({
  name: 'wat',

  rules: {
    root: $ => choice(repeat($.module), repeat1($._module_field)),

    addr_type: _ => choice('i32', 'i64'),

    array_type: $ => seq('(', 'array', $.field_type, ')'),

    block_block: $ =>
      choice(
        $._block_block_folded,
        seq(
          'block',
          optional($.identifier),
          optional($.type_use),
          repeat($.param),
          repeat($.result),
          repeat($._instr),
          'end',
          optional($.identifier),
        ),
      ),
    _block_block_folded: $ =>
      seq(
        '(',
        'block',
        optional($.identifier),
        optional($.type_use),
        repeat($.param),
        repeat($.result),
        repeat($._instr),
        ')',
      ),

    block_if: $ =>
      choice(
        $._block_if_folded,
        seq(
          'if',
          optional($.identifier),
          optional($.type_use),
          repeat($.param),
          repeat($.result),
          optional($.block_if_then),
          optional($.block_if_else),
          'end',
          optional($.identifier),
        ),
      ),
    _block_if_folded: $ =>
      seq(
        '(',
        'if',
        optional($.identifier),
        optional($.type_use),
        repeat($.param),
        repeat($.result),
        repeat($._instr),
        alias($._block_if_then_folded, $.block_if_then),
        optional(alias($._block_if_else_folded, $.block_if_else)),
        ')',
      ),
    block_if_else: $ => seq('else', optional($.identifier), repeat($._instr)),
    _block_if_else_folded: $ =>
      seq(
        '(',
        'else',
        repeat($._instr),
        ')',
      ),
    block_if_then: $ => repeat1($._instr),
    _block_if_then_folded: $ =>
      seq(
        '(',
        'then',
        repeat($._instr),
        ')',
      ),

    block_loop: $ =>
      choice(
        $._block_loop_folded,
        seq(
          'loop',
          optional($.identifier),
          optional($.type_use),
          repeat($.param),
          repeat($.result),
          repeat($._instr),
          'end',
          optional($.identifier),
        ),
      ),
    _block_loop_folded: $ =>
      seq(
        '(',
        'loop',
        optional($.identifier),
        optional($.type_use),
        repeat($.param),
        repeat($.result),
        repeat($._instr),
        ')',
      ),

    block_try_table: $ =>
      choice(
        $._block_try_table_folded,
        seq(
          'try_table',
          optional($.identifier),
          optional($.type_use),
          repeat($.param),
          repeat($.result),
          repeat($.catch),
          repeat($._instr),
          'end',
          optional($.identifier),
        ),
      ),
    _block_try_table_folded: $ =>
      seq(
        '(',
        'try_table',
        optional($.identifier),
        optional($.type_use),
        repeat($.param),
        repeat($.result),
        repeat($.catch),
        repeat($._instr),
        ')',
      ),

    _call_indirect: $ =>
      choice(
        $._call_indirect_folded,
        seq(
          alias(choice('call_indirect', 'return_call_indirect'), $.instr_name),
          optional(choice(
            seq($.type_use, repeat($.param), repeat($.result)),
            seq(repeat1($.param), repeat($.result)),
            repeat1($.result),
          )),
        ),
      ),
    _call_indirect_folded: $ =>
      seq(
        '(',
        alias(choice('call_indirect', 'return_call_indirect'), $.instr_name),
        optional(choice(
          seq($.type_use, repeat($.param), repeat($.result)),
          seq(repeat1($.param), repeat($.result)),
          repeat1($.result),
        )),
        repeat($._instr_folded),
        ')',
      ),

    catch: $ =>
      seq(
        '(',
        choice(
          seq(choice('catch', 'catch_ref'), $.index, $.index),
          seq(choice('catch_all', 'catch_all_ref'), $.index),
        ),
        ')',
      ),

    _composite_type: $ => choice($.func_type, $.struct_type, $.array_type, $.cont_type),

    cont_type: $ => seq('(', 'cont', $.index, ')'),

    data: $ => seq('(', 'data', repeat($.string), ')'),

    elem: $ => seq('(', 'elem', choice(repeat($.index), repeat($.elem_expr)), ')'),

    elem_expr: $ => choice(seq('(', 'item', repeat($._instr), ')'), $._instr_folded),

    elem_list: $ =>
      choice(
        seq('func', repeat($.index)),
        repeat1($.index),
        seq($.ref_type, repeat($.elem_expr)),
      ),

    export: $ => seq('(', 'export', $.string, ')'),

    extern_idx: $ => seq('(', choice('func', 'global', 'table', 'memory', 'tag'), $.index, ')'),

    _extern_type: $ =>
      choice(
        $.extern_type_func,
        $.extern_type_global,
        $.extern_type_memory,
        $.extern_type_table,
        $.extern_type_tag,
      ),
    extern_type_func: $ =>
      seq(
        '(',
        'func',
        optional($.identifier),
        optional($.type_use),
        repeat($.param),
        repeat($.result),
        ')',
      ),
    extern_type_global: $ => seq('(', 'global', optional($.identifier), $.global_type, ')'),
    extern_type_memory: $ => seq('(', 'memory', optional($.identifier), $.mem_type, ')'),
    extern_type_table: $ => seq('(', 'table', optional($.identifier), $.table_type, ')'),
    extern_type_tag: $ =>
      seq('(', 'tag', optional($.identifier), optional($.type_use), repeat($.param), ')'),

    field: $ =>
      seq(
        '(',
        'field',
        choice(seq($.identifier, $.field_type), repeat($.field_type)),
        ')',
      ),

    field_type: $ => choice(seq('(', 'mut', $._storage_type, ')'), $._storage_type),

    func_type: $ => seq('(', 'func', repeat($.param), repeat($.result), ')'),

    global_type: $ => choice($._value_type, seq('(', 'mut', $._value_type, ')')),

    heap_type: $ =>
      choice(
        'any',
        'eq',
        'i31',
        'struct',
        'array',
        'none',
        'func',
        'nofunc',
        'exn',
        'noexn',
        'extern',
        'noextern',
        'cont',
        'nocont',
        $.index,
      ),

    _immediate: $ =>
      choice(
        $.integer,
        $.float,
        $.identifier,
        $.string,
        $.shape_descriptor,
        $.mem_arg,
        $.ref_type,
        $.heap_type,
        $.on_clause,
      ),

    import: $ => seq('(', 'import', $.string, $.string, ')'),

    index: $ => choice($.identifier, $.uinteger),

    _instr: $ =>
      choice(
        $.block_block,
        $.block_loop,
        $.block_if,
        $.block_try_table,
        $.plain_instr,
        alias($._call_indirect, $.plain_instr),
      ),
    _instr_folded: $ =>
      choice(
        alias($._block_block_folded, $.block_block),
        alias($._block_loop_folded, $.block_loop),
        alias($._block_if_folded, $.block_if),
        alias($._block_try_table_folded, $.block_try_table),
        alias($._plain_instr_folded, $.plain_instr),
        alias($._call_indirect_folded, $.plain_instr),
      ),

    local: $ =>
      seq(
        '(',
        'local',
        choice(seq($.identifier, $._value_type), repeat($._value_type)),
        ')',
      ),

    mem_page_size: $ => seq('(', 'pagesize', $.uinteger, ')'),

    mem_type: $ =>
      seq(
        optional($.addr_type),
        field('min', $.uinteger),
        field('max', optional($.uinteger)),
        optional($.share),
        optional($.mem_page_size),
      ),

    mem_use: $ => seq('(', 'memory', $.index, ')'),

    module: $ => seq('(', 'module', optional($.identifier), repeat($._module_field), ')'),

    _module_field: $ =>
      choice(
        $.module_field_data,
        $.module_field_elem,
        $.module_field_export,
        $.module_field_func,
        $.module_field_global,
        $.module_field_import,
        $.module_field_memory,
        $.module_field_start,
        $.module_field_table,
        $.module_field_tag,
        $.rec_type,
        $.type_def,
      ),

    module_field_data: $ =>
      seq(
        '(',
        'data',
        optional($.identifier),
        optional($.mem_use),
        optional($.offset),
        repeat($.string),
        ')',
      ),

    module_field_elem: $ =>
      seq(
        '(',
        'elem',
        choice(
          seq(
            optional($.identifier),
            choice(optional('declare'), seq(optional($.table_use), $.offset)),
            optional($.elem_list),
          ),
          seq($.offset, optional($.elem_list)),
        ),
        ')',
      ),

    module_field_export: $ => seq('(', 'export', $.string, $.extern_idx, ')'),

    module_field_func: $ =>
      seq(
        '(',
        'func',
        optional($.identifier),
        repeat($.export),
        optional($.import),
        optional($.type_use),
        repeat($.param),
        repeat($.result),
        repeat($.local),
        repeat($._instr),
        ')',
      ),

    module_field_global: $ =>
      seq(
        '(',
        'global',
        optional($.identifier),
        repeat($.export),
        optional($.import),
        $.global_type,
        repeat($._instr),
        ')',
      ),

    module_field_import: $ => seq('(', 'import', $.string, $.string, $._extern_type, ')'),

    module_field_memory: $ =>
      seq(
        '(',
        'memory',
        optional($.identifier),
        repeat($.export),
        optional($.import),
        choice($.mem_type, $.data),
        ')',
      ),

    module_field_start: $ => seq('(', 'start', $.index, ')'),

    module_field_table: $ =>
      seq(
        '(',
        'table',
        optional($.identifier),
        repeat($.export),
        optional($.import),
        choice(
          seq($.table_type, repeat($._instr)),
          seq($.ref_type, $.elem),
        ),
        ')',
      ),

    module_field_tag: $ =>
      seq(
        '(',
        'tag',
        optional($.identifier),
        repeat($.export),
        optional($.import),
        optional($.type_use),
        repeat($.param),
        ')',
      ),

    num_type: _ => choice('i32', 'i64', 'f32', 'f64'),

    offset: $ => choice(seq('(', 'offset', repeat($._instr), ')'), $._instr_folded),

    on_clause: $ => seq('(', 'on', $.index, choice($.index, 'switch'), ')'),

    packed_type: _ => choice('i8', 'i16'),

    param: $ =>
      seq(
        '(',
        'param',
        choice(seq($.identifier, $._value_type), repeat($._value_type)),
        ')',
      ),

    plain_instr: $ =>
      choice(
        $._plain_instr_folded,
        seq($.instr_name, repeat($._immediate)),
      ),
    _plain_instr_folded: $ =>
      seq('(', $.instr_name, repeat($._immediate), repeat($._instr_folded), ')'),

    rec_type: $ => seq('(', 'rec', repeat($.type_def), ')'),

    ref_type: $ =>
      choice(
        'anyref',
        'eqref',
        'i31ref',
        'structref',
        'arrayref',
        'nullref',
        'funcref',
        'nullfuncref',
        'exnref',
        'nullexnref',
        'externref',
        'nullexternref',
        'contref',
        'nullcontref',
        seq('(', 'ref', optional('null'), $.heap_type, ')'),
      ),

    result: $ => seq('(', 'result', repeat($._value_type), ')'),

    _storage_type: $ => choice($._value_type, $.packed_type),

    struct_type: $ => seq('(', 'struct', repeat($.field), ')'),

    sub_type: $ =>
      choice(
        $._composite_type,
        seq('(', 'sub', optional('final'), repeat($.index), $._composite_type, ')'),
      ),

    table_type: $ =>
      seq(
        optional($.addr_type),
        field('min', $.uinteger),
        field('max', optional($.uinteger)),
        $.ref_type,
      ),

    table_use: $ => seq('(', 'table', $.index, ')'),

    type_def: $ => seq('(', 'type', optional($.identifier), $.sub_type, ')'),

    type_use: $ => seq('(', 'type', $.index, ')'),

    _value_type: $ => choice($.num_type, $.vec_type, $.ref_type),

    vec_type: _ => 'v128',

    // tokens
    identifier: _ => token(/\$(?:[a-z\d!#$%&'*+-./:<=>?@\\\^_`|~]+|"[^"\r\n]*")/i),
    instr_name: _ => token(/[a-z_]+(?:[a-z\d\._]+)?/i),
    integer: _ => token(/[+-]?(?:\d+(?:_\d+)*\d*|0x[\da-fA-F]+(?:_[\da-fA-F]+)*[\da-fA-F]*)/),
    float: _ =>
      choice(
        token(
          /[+-]?((\d+(_\d+)*\d*\.(\d+(_\d+)*\d*)?([Ee][+-]?\d+(_\d+)*\d*)?|0x[\da-fA-F]+(_[\da-fA-F]+)*[\da-fA-F]*\.([\da-fA-F]+(_[\da-fA-F]+)*[\da-fA-F]*)?([Pp][+-]?\d+(_\d+)*\d*)?)|inf|nan(\:0x[\da-fA-F]+(_[\da-fA-F])*[\da-fA-F]*)?)/,
        ),
        token(
          /[+-]?(\d+(_\d+)*\d*[Ee]|0x[\da-fA-F]+(_[\da-fA-F]+)*[\da-fA-F]*[Pp])[+-]?\d+(_\d+)*\d*/,
        ),
      ),
    mem_arg: _ =>
      token(/(?:align|offset)=(?:\d(?:_\d+)*\d*|0x[\da-fA-F](?:_[\da-fA-F])*[\da-fA-F]*)/),
    shape_descriptor: _ =>
      choice(
        token('i8x16'),
        token('i16x8'),
        token('i32x4'),
        token('i64x2'),
        token('f32x4'),
        token('f64x2'),
      ),
    share: _ => choice(token('shared'), token('unshared')),
    string: _ => token(/"(?:[^"\r\n]|\\")*"/),
    uinteger: _ => token(/(?:\d+(?:_\d+)*\d*|0x[\da-fA-F]+(?:_[\da-fA-F])*[\da-fA-F]*)/),

    // trivias
    _annotation: $ => seq($.annotation_start, repeat($.annotation_elem), $.annotation_end),
    annotation_start: _ => token(/\(\@(?:[a-z\d!#$%&'*+-./:<=>?@\\\^_`|~]+|"(?:[^"\r\n]|\\")*")/i),
    annotation_elem: _ => token(/[^)\s]+/),
    annotation_end: _ => token(')'),
    block_comment: $ => seq('(;', optional($._block_comment_content), ';)'),
    line_comment: _ => token(/;;[^\r\n]*/),
  },

  extras: $ => [/\s+/, $.block_comment, $.line_comment, $._annotation],

  externals: $ => [$._block_comment_content],

  conflicts: $ => [
    [$.plain_instr],
    [$.index, $._immediate],
    [$._call_indirect],
    [$.index, $.module_field_elem],
  ],
})
