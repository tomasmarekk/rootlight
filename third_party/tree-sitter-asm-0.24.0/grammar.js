// Physical Assembly owners extend the retained upstream instruction grammar.
// MASM containers preserve written boundaries; assembler binding and expansion
// are not inferred from these source nodes.
module.exports = grammar({
    name: 'asm',
    extras: $ => [
        / |\t|\r/,
        $.line_comment,
        $.block_comment,
    ],
    conflicts: $ => [
        [$.masm_body],
        [
            $._expr,
            $._tc_expr,
        ],
    ],

    rules: {
        program: $ => seq(repeat('\n'), sep(repeat1('\n'), $._item)),
        _item: $ =>
            choice(
                $.procedure_definition,
                $.macro_definition,
                $.segment_definition,
                $.data_definition,
                $.constant_definition,
                $.meta,
                $.label,
                $.const,
                $.instruction,
            ),

        procedure_definition: $ =>
            seq(
                field('name', $.ident),
                field('header', $.procedure_header),
                repeat1('\n'),
                optional(field('body', $.masm_body)),
                field('end_name', $.ident),
                $._masm_endp,
            ),
        procedure_header: $ =>
            seq($._masm_proc, optional(choice($._masm_near, $._masm_far))),
        macro_definition: $ =>
            seq(
                field('name', $.ident),
                field('header', $.macro_header),
                repeat1('\n'),
                optional(field('body', $.masm_body)),
                $._masm_endm,
            ),
        macro_header: $ =>
            seq($._masm_macro, optional(field('parameters', $.masm_parameters))),
        masm_parameters: $ => seq($.ident, repeat(seq(',', $.ident))),
        segment_definition: $ =>
            seq(
                field('name', $.ident),
                field('header', $.segment_header),
                repeat1('\n'),
                optional(field('body', $.masm_body)),
                field('end_name', $.ident),
                $._masm_ends,
            ),
        segment_header: $ => $._masm_segment,
        // Newlines remain syntax: a closer is never part of the captured body.
        masm_body: $ =>
            seq($._item, repeat(seq(repeat1('\n'), $._item)), repeat1('\n')),
        data_definition: $ =>
            seq(
                field('name', $.ident),
                field('type', $._masm_db),
                field('value', $.masm_initializer),
            ),
        masm_initializer: $ =>
            seq($._masm_initializer_value, repeat(seq(',', $._masm_initializer_value))),
        _masm_initializer_value: $ => choice($._tc_expr, $.float, $.masm_string),
        masm_string: $ => /'[^'\r\n]*'/,
        constant_definition: $ =>
            seq(field('name', $.ident), $._masm_equ, field('value', $._tc_expr)),

        // These markers are valid only in MASM owner/header states. Keeping the
        // ordinary token precedence preserves longest-match identifier spelling.
        _masm_proc: $ => token(/[Pp][Rr][Oo][Cc]/),
        _masm_endp: $ => token(/[Ee][Nn][Dd][Pp]/),
        _masm_near: $ => token(/[Nn][Ee][Aa][Rr]/),
        _masm_far: $ => token(/[Ff][Aa][Rr]/),
        _masm_macro: $ => token(/[Mm][Aa][Cc][Rr][Oo]/),
        _masm_endm: $ => token(/[Ee][Nn][Dd][Mm]/),
        _masm_segment: $ => token(/[Ss][Ee][Gg][Mm][Ee][Nn][Tt]/),
        _masm_ends: $ => token(/[Ee][Nn][Dd][Ss]/),
        _masm_db: $ => token(/[Dd][Bb]/),
        _masm_equ: $ => token(/[Ee][Qq][Uu]/),

        meta: $ =>
            seq(
                field('kind', $.meta_ident),
                optional(choice(
                    $.ident,
                    seq($.int, repeat(seq(',', $.int))),
                    seq($.float, repeat(seq(',', $.float))),
                    seq($.string, repeat(seq(',', $.string))),
                )),
            ),
        label: $ =>
            choice(
                seq(
                    choice($.meta_ident, alias($.word, $.ident), alias($._ident, $.ident)),
                    ':',
                    optional(seq('(', $.ident, ')')),
                ),
                seq(
                    'label',
                    field('name', $.word),
                ),
            ),
        const: $ => seq('const', field('name', $.word), field('value', $._tc_expr)),
        instruction: $ => seq(field('kind', $.word), choice(sep(',', $._expr), repeat($._tc_expr))),

        _expr: $ => choice($.ptr, $.ident, $.int, $.string, $.float),
        ptr: $ =>
            choice(
                seq(
                    optional(seq(choice('byte', 'word', 'dword', 'qword'), 'ptr')),
                    '[',
                    $.reg,
                    optional(seq(choice('+', '-'), choice($.int, $.ident))),
                    ']',
                ),
                seq(
                    optional($.int),
                    '(',
                    $.reg,
                    ')',
                ),
                seq(
                    '*',
                    'rel',
                    '[',
                    $.int,
                    ']',
                ),
                // Aarch64
                seq(
                    '[',
                    $.reg,
                    optional(seq(',', $.int)),
                    ']',
                    optional('!'),
                ),
            ),
        // Turing Complete
        _tc_expr: $ =>
            choice(
                $.ident,
                $.int,
                $.string,
                $.tc_infix,
            ),
        tc_infix: $ =>
            choice(
                ...[
                    ['+', 0],
                    ['-', 0],
                    ['*', 1],
                    ['/', 1],
                    ['%', 1],
                    ['|', 2],
                    ['^', 3],
                    ['&', 4],
                ].map(([op, p]) =>
                    prec.left(
                        p,
                        seq(field('lhs', $._tc_expr), field('op', op), field('rhs', $._tc_expr)),
                    )
                ),
            ),

        int: $ => {
            const _int = /-?([0-9][0-9_]*|(0x|\$)[0-9A-Fa-f][0-9A-Fa-f_]*|0b[01][01_]*)/
            return choice(
                seq('#', token.immediate(_int)),
                _int,
            )
        },
        float: $ => /-?[0-9][0-9_]*\.([0-9][0-9_]*)?/,
        string: $ => /"[^"]*"/,

        word: $ => /[a-zA-Z0-9_]+/,
        _reg: $ => /%?[a-z0-9]+/,
        address: $ => /\$[a-zA-Z0-9_]+/, // GAS x86 address
        reg: $ => choice($._reg, $.word, $.address),
        meta_ident: $ => /\.[a-z_]+/,
        _ident: $ => /[a-zA-Z_0-9.]+/,
        ident: $ => choice($._ident, $.meta_ident, $.reg),

        line_comment: $ =>
            choice(
                seq('#', token.immediate(/.*/)),
                /(\/\/|;).*/,
            ),
        block_comment: $ =>
            token(seq(
                '/*',
                /[^*]*\*+([^/*][^*]*\*+)*/,
                '/',
            )),
    },
})

function sep(separator, rule) {
    return optional(seq(rule, repeat(seq(separator, rule)), optional(separator)))
}
