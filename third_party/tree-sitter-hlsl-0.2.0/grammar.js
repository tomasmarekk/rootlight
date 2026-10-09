const CPP = require("tree-sitter-cpp/grammar")

module.exports = grammar(CPP, {
    name: 'hlsl',

    conflicts: ($, original) => original.concat([
        [$.function_declarator],
        [$._top_level_item],
    ]),

    rules: {
        _top_level_item: ($, original) => choice(
            seq($.cbuffer_specifier, optional(';')),
            original
        ),

        function_definition: ($, original) => seq(
            optional(
                $.hlsl_attribute,
            )
            , original
        ),
        function_declarator: ($, original) => seq(
            original,
            optional($.semantics)
        ),

        declaration: $ => seq(
            $._declaration_specifiers,
            commaSep1(field('declarator', choice(
                seq($._declarator, optional(alias(seq(':', $.expression), $.semantics))),
                $.init_declarator
            ))),
            ';'
        ),

        _declaration_modifiers: ($, original) => choice(
            'in',
            'out',
            'inout',
            $.qualifiers,
            original),


        parameter_declaration: ($, original) =>
            seq(
                original,
                optional($.semantics),
            ),

        semantics: $ => seq(":", $.identifier),

        _non_case_statement: ($, original) => choice($.discard_statement, $.cbuffer_specifier, original),

        if_statement: ($, original) => seq(optional($.hlsl_attribute), original),

        discard_statement: _ => seq('discard', ';'),
        qualifiers: _ => choice(
            'precise',
            'shared',
            'groupshared',
            'uniform',
            'row_major',
            'column_major',
            'globallycoherent',
            'centroid',
            'noperspective',
            'nointerpolation',
            'sample',
            'linear',
            'snorm',
            'unorm',
            'point',
            'line',
            'triangleadj',
            'lineadj',
            'triangle',
        ),

        cbuffer_specifier: $ => prec.right(seq(
            'cbuffer',
            optional($.attribute_declaration),
            choice(
                field('name', $._class_name),
                seq(
                    optional(field('name', $._class_name)),
                    optional($.virtual_specifier),
                    optional(choice(
                        field('binding', $.resource_binding),
                        $.base_class_clause
                    )),
                    field('body', $.field_declaration_list)
                )
            )
        )),

        resource_binding: $ => seq(
            ':', 'register', '(',
            field('register', alias(/[tubscTUBSC][0-9]+/, $.identifier)),
            optional(seq(',', field('space', alias(/space[0-9]+/, $.identifier)))),
            ')'
        ),

        hlsl_attribute: $ => seq('[',
            $.expression,
            ']'),

        for_statement: ($, original) => seq(optional($.hlsl_attribute), original),

    }
});

function commaSep1(rule) {
    return seq(rule, repeat(seq(',', rule)))
}
