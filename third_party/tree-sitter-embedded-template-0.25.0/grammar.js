/**
 * @file ERB delimiter profile of the retained Embedded Template grammar
 * @author Max Brunsfeld <maxbrunsfeld@gmail.com>
 * @license MIT
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

module.exports = grammar({
  name: 'embedded_template',

  extras: _ => [],

  rules: {
    template: $ => repeat(choice(
      $.directive,
      $.output_directive,
      $.comment_directive,
      $.content,
    )),

    code: _ => repeat1(choice(/[^%=_-]+|[%=_-]/, '%%>')),

    content: _ => prec.right(repeat1(choice(/[^<]+|</, '<%%'))),

    directive: $ => seq(
      choice('<%', '<%-'),
      optional($.code),
      choice('%>', '-%>'),
    ),

    output_directive: $ => seq(
      choice('<%=', '<%=='),
      optional($.code),
      choice('%>', '-%>'),
    ),

    comment_directive: $ => seq(
      '<%#',
      optional(alias($.code, $.comment)),
      choice('%>', '-%>'),
    ),
  },
});
