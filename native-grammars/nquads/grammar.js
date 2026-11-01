// Native RDF 1.1 N-Quads syntax with physical term and statement boundaries.
// Unicode name/escape ranges derive from the reviewed MIT Turtle lexical rules;
// line structure and absolute-IRI constraints follow the N-Quads Recommendation.

const UCHAR = choice(
  /\\u(?:[0-9A-Ca-cE-Fe-f][0-9A-Fa-f]{3}|[Dd][0-7][0-9A-Fa-f]{2})/,
  /\\U(?:0000(?:[0-9A-Ca-cE-Fe-f][0-9A-Fa-f]{3}|[Dd][0-7][0-9A-Fa-f]{2})|000[1-9A-Fa-f][0-9A-Fa-f]{4}|0010[0-9A-Fa-f]{4})/,
);
const PN_CHARS_BASE = /[A-Za-z\u00C0-\u00D6\u00D8-\u00F6\u00F8-\u02FF\u0370-\u037D\u037F-\u1FFF\u200C-\u200D\u2070-\u218F\u2C00-\u2FEF\u3001-\uD7FF\uF900-\uFDCF\uFDF0-\uFFFD\u{10000}-\u{EFFFF}]/u;
const PN_CHARS_U = choice(PN_CHARS_BASE, '_', ':');
const PN_CHARS = choice(PN_CHARS_U, '-', /[0-9\u00B7\u0300-\u036F\u203F-\u2040]/);

function escapedAscii(characters) {
  return characters.split('').flatMap(character => {
    const digits = character.charCodeAt(0).toString(16).padStart(4, '0')
      .replace(/[a-f]/g, digit => `[${digit}${digit.toUpperCase()}]`);
    return [new RegExp(`\\\\u${digits}`), new RegExp(`\\\\U0000${digits}`)];
  });
}

const SCHEME_FIRST = choice(/[A-Za-z]/, ...escapedAscii('ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz'));
const SCHEME_REST = choice(/[A-Za-z0-9+.-]/, ...escapedAscii('ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+.-'));

function iriReference() {
  return seq(
    '<', SCHEME_FIRST, repeat(SCHEME_REST), choice(':', ...escapedAscii(':')),
    repeat(choice(/[^\x00-\x20<>"{}|^`\\]/u, UCHAR)), '>',
  );
}

module.exports = grammar({
  name: 'nquads',

  externals: $ => [$.string, $.comment],

  extras: $ => [/[ \t]/, $.comment],

  rules: {
    document: $ => seq(
      repeat(seq(optional($.quad), $._end_of_line)),
      optional($.quad),
    ),

    quad: $ => seq(
      field('subject', choice($.iri_reference, $.blank_node_label)),
      field('predicate', $.iri_reference),
      field('object', choice($.iri_reference, $.blank_node_label, $.literal)),
      optional(field('graph', choice($.iri_reference, $.blank_node_label))),
      '.',
    ),

    iri_reference: _ => token(iriReference()),

    blank_node_label: _ => token(seq(
      '_:', choice(PN_CHARS_U, /[0-9]/),
      optional(seq(repeat(choice(PN_CHARS, '.')), PN_CHARS)),
    )),

    literal: $ => seq(
      field('value', $.string),
      optional(choice(
        seq(token.immediate('^^'), field('datatype', alias(token.immediate(iriReference()), $.iri_reference))),
        field('language', $.language_tag),
      )),
    ),

    language_tag: _ => token.immediate(/@[A-Za-z]+(-[A-Za-z0-9]+)*/),
    _end_of_line: _ => /[\r\n]+/,
  },
});
