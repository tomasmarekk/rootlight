#include "tree_sitter/parser.h"

enum TokenType {
    DESCENDANT_OP,
    PSEUDO_CLASS_SELECTOR_COLON,
    ERROR_RECOVERY,
};

static inline void advance(TSLexer *lexer) { lexer->advance(lexer, false); }

static inline void skip(TSLexer *lexer) { lexer->advance(lexer, true); }

// CSS whitespace and identifier starts are independent of the process locale.
static inline bool is_css_whitespace(int32_t codepoint) {
    return codepoint == ' ' || codepoint == '\t' || codepoint == '\n' || codepoint == '\r' || codepoint == '\f';
}

static inline bool starts_identifier(int32_t codepoint) {
    return (codepoint >= 'a' && codepoint <= 'z') || (codepoint >= 'A' && codepoint <= 'Z') ||
           codepoint == '_' || codepoint == '\\' || codepoint >= 0x80;
}

void *tree_sitter_css_external_scanner_create() { return NULL; }

void tree_sitter_css_external_scanner_destroy(void *payload) {}

unsigned tree_sitter_css_external_scanner_serialize(void *payload, char *buffer) { return 0; }

void tree_sitter_css_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {}

bool tree_sitter_css_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
    if (valid_symbols[ERROR_RECOVERY]) {
        return false;
    }

    if (is_css_whitespace(lexer->lookahead) && valid_symbols[DESCENDANT_OP]) {
        lexer->result_symbol = DESCENDANT_OP;

        skip(lexer);
        while (is_css_whitespace(lexer->lookahead)) {
            skip(lexer);
        }
        lexer->mark_end(lexer);

        if (lexer->lookahead == '#' || lexer->lookahead == '.' || lexer->lookahead == '[' || lexer->lookahead == '-' ||
            lexer->lookahead == '*' || starts_identifier(lexer->lookahead)) {
            return true;
        }

        if (lexer->lookahead == ':') {
            advance(lexer);
            if (is_css_whitespace(lexer->lookahead)) {
                return false;
            }
            for (;;) {
                if (lexer->lookahead == ';' || lexer->lookahead == '}' || lexer->eof(lexer)) {
                    return false;
                }
                if (lexer->lookahead == '{') {
                    return true;
                }
                advance(lexer);
            }
        }
    }

    if (valid_symbols[PSEUDO_CLASS_SELECTOR_COLON]) {
        while (is_css_whitespace(lexer->lookahead)) {
            skip(lexer);
        }
        if (lexer->lookahead == ':') {
            advance(lexer);
            if (lexer->lookahead == ':') {
                return false;
            }
            lexer->mark_end(lexer);
            lexer->result_symbol = PSEUDO_CLASS_SELECTOR_COLON;

            // We need a `{` to be a pseudo class selector, a `;` indicates a property.
            // This does not apply if we're in a comment, however.
            bool in_comment = false;
            while (lexer->lookahead != ';' && lexer->lookahead != '}' && !lexer->eof(lexer)) {
                advance(lexer);
                if (lexer->lookahead == '{' && !in_comment) {
                    return true;
                }
                if (lexer->lookahead == '/' && !in_comment) {
                    advance(lexer);
                    if (lexer->lookahead == '*') {
                        in_comment = true;
                    }
                } else if (lexer->lookahead == '*' && in_comment) {
                    advance(lexer);
                    if (lexer->lookahead == '/') {
                        in_comment = false;
                    }
                }
            }

            // If we're at eof, and we happened to *not* find an opening brace to indicate we have a pseudo class
            // selector, we should *still* return one at EOF. This will improve error recovery, and the malformed code
            // can be parsed as an erroneous pseudo-class selector, rather than an erroneous property.
            return lexer->eof(lexer);
        }
    }

    return false;
}
