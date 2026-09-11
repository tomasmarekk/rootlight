// External Groovy trivia and slash dispatch, preserving comments as source nodes.
// A continuation decision is cached only across its already inspected trivia.
// Serialized distance lets incremental parsing distinguish changed lookahead.
#include "tree_sitter/parser.h"
#include <stdint.h>
#include <stdlib.h>

enum TokenType {
    SLASHY_STRING_START, LINE_COMMENT, BLOCK_COMMENT, GROOVYDOC_COMMENT,
    NEWLINE, LINE_CONTINUATION, DIVISION, STRING_CONTENT_GUARD,
};

typedef struct { uint32_t remaining; uint8_t decision; } Scanner;
typedef struct {
    TSLexer *lexer;
    uint64_t consumed;
    uint64_t token_end;
} Cursor;

static void advance(Cursor *cursor, bool skip) {
    cursor->lexer->advance(cursor->lexer, skip);
    cursor->consumed++;
}
static void mark_end(Cursor *cursor) {
    cursor->lexer->mark_end(cursor->lexer);
    cursor->token_end = cursor->consumed;
}
static bool whitespace(int32_t c) {
    return c == ' ' || c == '\t' || c == '\n' || c == '\r';
}
static bool word_character(int32_t c) {
    return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')
        || (c >= '0' && c <= '9') || c == '_' || c == '$' || c > 127;
}
static bool keyword(Cursor *cursor, const char *suffix) {
    while (*suffix) {
        if (cursor->lexer->lookahead != *suffix++) return false;
        advance(cursor, false);
    }
    return !word_character(cursor->lexer->lookahead);
}

// Returns the first non-trivia character. A slash may already be consumed to
// distinguish a division operator from a comment opener, but no token is emitted.
static int32_t after_trivia(Cursor *cursor, uint64_t *last_boundary) {
    TSLexer *lexer = cursor->lexer;
    for (;;) {
        while (whitespace(lexer->lookahead)) {
            int32_t c = lexer->lookahead;
            advance(cursor, false);
            if (c == '\n' || c == '\r') *last_boundary = cursor->consumed;
        }
        if (lexer->lookahead != '/') return lexer->lookahead;
        advance(cursor, false);
        if (lexer->lookahead == '/') {
            while (!lexer->eof(lexer) && lexer->lookahead != '\n' && lexer->lookahead != '\r') advance(cursor, false);
            *last_boundary = cursor->consumed;
        } else if (lexer->lookahead == '*') {
            advance(cursor, false);
            bool closed = false;
            while (!lexer->eof(lexer)) {
                int32_t c = lexer->lookahead;
                advance(cursor, false);
                if (c == '*' && lexer->lookahead == '/') {
                    advance(cursor, false);
                    closed = true;
                    break;
                }
            }
            if (!closed) return 0;
            *last_boundary = cursor->consumed;
        } else {
            return '/';
        }
    }
}
static bool continues_expression(Cursor *cursor, int32_t first) {
    TSLexer *lexer = cursor->lexer;
    if (first == '/') return true;
    switch (first) {
        case '.':
            advance(cursor, false);
            return !(lexer->lookahead >= '0' && lexer->lookahead <= '9');
        case '*':
            advance(cursor, false);
            if (lexer->lookahead != '*') return true;
            advance(cursor, false);
            return lexer->lookahead == '=';
        case '+': case '-':
            advance(cursor, false);
            return lexer->lookahead == '=';
        case '!':
            advance(cursor, false);
            if (lexer->lookahead == '=') return true;
            if (lexer->lookahead != 'i') return false;
            advance(cursor, false);
            if (lexer->lookahead != 'n') return false;
            advance(cursor, false);
            return !word_character(lexer->lookahead) || keyword(cursor, "stanceof");
        case 'i':
            advance(cursor, false);
            if (lexer->lookahead != 'n') return false;
            advance(cursor, false);
            return !word_character(lexer->lookahead) || keyword(cursor, "stanceof");
        case 'a':
            advance(cursor, false);
            return keyword(cursor, "s");
        case ':':
            advance(cursor, false);
            return lexer->lookahead == ':';
        case '%': case '<': case '>': case '=':
        case '&': case '|': case '^': case '?':
            return true;
        default:
            return false;
    }
}
static bool emit(Scanner *scanner, Cursor *cursor, enum TokenType token) {
    scanner->remaining = scanner->remaining > cursor->token_end
        ? scanner->remaining - (uint32_t)cursor->token_end : 0;
    if (!scanner->remaining) scanner->decision = 0;
    cursor->lexer->result_symbol = token;
    return true;
}
void *tree_sitter_groovy_external_scanner_create(void) {
    return calloc(1, sizeof(Scanner));
}
void tree_sitter_groovy_external_scanner_destroy(void *payload) {
    free(payload);
}
unsigned tree_sitter_groovy_external_scanner_serialize(void *payload, char *buffer) {
    const Scanner *scanner = payload;
    if (!scanner || !scanner->remaining) return 0;
    for (unsigned i = 0; i < 4; i++) buffer[i] = (char)(scanner->remaining >> (8 * i));
    buffer[4] = (char)scanner->decision;
    return 5;
}
void tree_sitter_groovy_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
    Scanner *scanner = payload;
    if (!scanner) return;
    scanner->remaining = 0;
    scanner->decision = 0;
    if (length != 5 || (buffer[4] != 1 && buffer[4] != 2)) return;
    for (unsigned i = 0; i < 4; i++) scanner->remaining |= (uint32_t)(uint8_t)buffer[i] << (8 * i);
    if (scanner->remaining) scanner->decision = (uint8_t)buffer[4];
}
bool tree_sitter_groovy_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
    Scanner *scanner = payload;
    if (!scanner) return false;
    // This token is never emitted. Its valid-symbol bit identifies literal-body
    // states where internal text/delimiter tokens must own slash and whitespace.
    // NEWLINE also being valid preserves the competing completed-slashy branch
    // after a terminal escaped slash. Embedded expressions admit normal trivia.
    if (valid_symbols[STRING_CONTENT_GUARD] && !valid_symbols[NEWLINE]) return false;
    Cursor cursor = { .lexer = lexer };
    bool saw_newline = false;
    while (whitespace(lexer->lookahead)) {
        if ((valid_symbols[NEWLINE] || valid_symbols[LINE_CONTINUATION])
            && (lexer->lookahead == '\n' || lexer->lookahead == '\r')) {
            saw_newline = true;
            advance(&cursor, false);
            mark_end(&cursor);
        } else {
            advance(&cursor, !saw_newline);
        }
    }
    if (saw_newline) {
        if (valid_symbols[LINE_CONTINUATION]) {
            if (scanner->remaining && scanner->remaining >= cursor.token_end) {
                enum TokenType token = scanner->decision == 1 ? LINE_CONTINUATION : NEWLINE;
                if (!valid_symbols[token]) return false;
                return emit(scanner, &cursor, token);
            }
            uint64_t last_boundary = cursor.token_end;
            int32_t first = after_trivia(&cursor, &last_boundary);
            bool continuation = continues_expression(&cursor, first);
            enum TokenType token = continuation ? LINE_CONTINUATION : NEWLINE;
            if (!valid_symbols[token]) return false;
            // Cache both outcomes until the last trivia token, never trailing
            // spaces: internal lexer tokens cannot decrement scanner distance.
            // First-token lookahead and serialized decisions bind incremental reuse.
            uint64_t remaining = last_boundary - cursor.token_end;
            scanner->remaining = remaining <= UINT32_MAX ? (uint32_t)remaining : 0;
            scanner->decision = scanner->remaining ? (continuation ? 1 : 2) : 0;
            lexer->result_symbol = token;
            return true;
        }
        if (!valid_symbols[NEWLINE]) return false;
        return emit(scanner, &cursor, NEWLINE);
    }
    if (lexer->lookahead != '/') return false;
    advance(&cursor, false);
    int32_t next = lexer->lookahead;
    if (next == '/') {
        if (!valid_symbols[LINE_COMMENT]) return false;
        while (!lexer->eof(lexer) && lexer->lookahead != '\n' && lexer->lookahead != '\r') advance(&cursor, false);
        mark_end(&cursor);
        return emit(scanner, &cursor, LINE_COMMENT);
    }
    if (next == '*') {
        advance(&cursor, false);
        bool doc = lexer->lookahead == '*';
        while (!lexer->eof(lexer)) {
            int32_t c = lexer->lookahead;
            advance(&cursor, false);
            if (c == '*' && lexer->lookahead == '/') {
                advance(&cursor, false);
                enum TokenType token = doc ? GROOVYDOC_COMMENT : BLOCK_COMMENT;
                if (!valid_symbols[token]) return false;
                mark_end(&cursor);
                return emit(scanner, &cursor, token);
            }
        }
        return false;
    }
    // When both a command argument and an operator are possible, Groovy treats
    // slash as division. Comments and compound assignment retain their own tokens.
    if (next != '=' && valid_symbols[DIVISION]) {
        mark_end(&cursor);
        return emit(scanner, &cursor, DIVISION);
    }
    if (!valid_symbols[SLASHY_STRING_START] || (next == '=' && valid_symbols[DIVISION])) return false;
    // Only classify the opener. Grammar tokens own multiline text, escapes and
    // interpolation; searching for a closer here duplicates work and mistakes
    // slashes inside embedded expressions for literal delimiters.
    mark_end(&cursor);
    return emit(scanner, &cursor, SLASHY_STRING_START);
}
