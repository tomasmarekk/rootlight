/* Shared XML/DTD lexical rules retain written source spelling without resolution.
 * Derived from the pinned upstream grammar; the retained MIT notice is in LICENSE.
 */
#pragma once

#include "tree_sitter/parser.h"

enum TokenType {
    PI_TARGET,
    PI_CONTENT,
    COMMENT,

#ifdef TS_XML
    CHAR_DATA,
    CDATA,
    XML_MODEL,
    XML_STYLESHEET,
    START_TAG_NAME,
    END_TAG_NAME,
    ERRONEOUS_END_NAME,
    SELF_CLOSING_TAG_DELIMITER,
#endif
};

#define advance_if_eq(lexer, chr) \
    do { \
        if ((lexer)->eof(lexer) || (lexer)->lookahead != (chr)) return false; \
        advance(lexer); \
    } while (0)

#ifdef _WIN32
#undef max
#undef min
#endif

static inline void advance(TSLexer *lexer) { lexer->advance(lexer, false); }

/* XML 1.0 fifth edition, productions [4] and [4a]; locale and wchar_t are irrelevant.
 * https://www.w3.org/TR/2008/REC-xml-20081126/#NT-NameStartChar
 */
static inline bool is_valid_name_start_char(int32_t chr) {
    return chr == ':' || chr == '_' ||
           (chr >= 'A' && chr <= 'Z') || (chr >= 'a' && chr <= 'z') ||
           (chr >= 0xC0 && chr <= 0xD6) || (chr >= 0xD8 && chr <= 0xF6) ||
           (chr >= 0xF8 && chr <= 0x2FF) || (chr >= 0x370 && chr <= 0x37D) ||
           (chr >= 0x37F && chr <= 0x1FFF) || (chr >= 0x200C && chr <= 0x200D) ||
           (chr >= 0x2070 && chr <= 0x218F) || (chr >= 0x2C00 && chr <= 0x2FEF) ||
           (chr >= 0x3001 && chr <= 0xD7FF) || (chr >= 0xF900 && chr <= 0xFDCF) ||
           (chr >= 0xFDF0 && chr <= 0xFFFD) || (chr >= 0x10000 && chr <= 0xEFFFF);
}

static inline bool is_valid_name_char(int32_t chr) {
    return is_valid_name_start_char(chr) || chr == '-' || chr == '.' ||
           (chr >= '0' && chr <= '9') || chr == 0xB7 ||
           (chr >= 0x300 && chr <= 0x36F) || (chr >= 0x203F && chr <= 0x2040);
}

static inline bool is_valid_xml_char(int32_t chr) {
    return chr == 0x9 || chr == 0xA || chr == 0xD ||
           (chr >= 0x20 && chr <= 0xD7FF) || (chr >= 0xE000 && chr <= 0xFFFD) ||
           (chr >= 0x10000 && chr <= 0x10FFFF);
}

static bool scan_pi_target(TSLexer *lexer, const bool *valid_symbols) {
    if (lexer->eof(lexer) || !is_valid_name_start_char(lexer->lookahead)) return false;

    const char reserved[] = "xml";
    bool reserved_match = true;
    unsigned length = 0;
#ifdef TS_XML
    const char model[] = "xml-model";
    const char stylesheet[] = "xml-stylesheet";
    bool model_match = true, stylesheet_match = true;
#else
    (void)valid_symbols;
#endif

    do {
        int32_t chr = lexer->lookahead;
        reserved_match = reserved_match && length < sizeof reserved - 1 &&
                         (chr == reserved[length] || chr == reserved[length] - ('a' - 'A'));
#ifdef TS_XML
        model_match = model_match && length < sizeof model - 1 && chr == model[length];
        stylesheet_match = stylesheet_match && length < sizeof stylesheet - 1 && chr == stylesheet[length];
#endif
        /* Only fixed ASCII targets need a length; long names must not size memory or wrap it. */
        if (length < 15) ++length;
        advance(lexer);
    } while (!lexer->eof(lexer) && is_valid_name_char(lexer->lookahead));

    if (reserved_match && length == sizeof reserved - 1) return false;
    lexer->mark_end(lexer);
#ifdef TS_XML
    if (model_match && length == sizeof model - 1 && valid_symbols[XML_MODEL]) {
        lexer->result_symbol = XML_MODEL;
        return true;
    }
    if (stylesheet_match && length == sizeof stylesheet - 1 && valid_symbols[XML_STYLESHEET]) {
        lexer->result_symbol = XML_STYLESHEET;
        return true;
    }
    if (!valid_symbols[PI_TARGET]) return false;
#endif
    lexer->result_symbol = PI_TARGET;
    return true;
}

static bool scan_pi_content(TSLexer *lexer) {
    bool has_content = false;
    while (!lexer->eof(lexer)) {
        if (!is_valid_xml_char(lexer->lookahead)) return false;
        if (lexer->lookahead == '?') {
            lexer->mark_end(lexer);
            advance(lexer);
            if (lexer->lookahead == '>') {
                lexer->result_symbol = PI_CONTENT;
                return has_content;
            }
        } else {
            advance(lexer);
        }
        has_content = true;
        lexer->mark_end(lexer);
    }
    return false;
}

static bool scan_comment(TSLexer *lexer) {
    advance_if_eq(lexer, '-');
    advance_if_eq(lexer, '-');
    while (!lexer->eof(lexer)) {
        if (!is_valid_xml_char(lexer->lookahead)) return false;
        if (lexer->lookahead == '-') {
            advance(lexer);
            if (lexer->lookahead == '-') {
                advance(lexer);
                advance_if_eq(lexer, '>');
                lexer->mark_end(lexer);
                lexer->result_symbol = COMMENT;
                return true;
            }
        } else {
            advance(lexer);
        }
    }
    return false;
}
