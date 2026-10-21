/* Bounded derivative of zealot128/tree-sitter-pug 13e91953, MIT.
 * Pristine source stays separate. Refusal is sticky outside serialized state,
 * so recovery cannot turn incomplete scanner state into admitted syntax. */
#include "tree_sitter/parser.h"
#include "rootlight_pug_status.h"
#include <limits.h>
#include <string.h>

enum TokenType { NEWLINE, INDENT, DEDENT };
enum { PUG_WIRE_HEADER = 4, PUG_MAX_INDENTS = 510, PUG_WIRE_VERSION = 1 };
_Static_assert(PUG_WIRE_HEADER + 2 * PUG_MAX_INDENTS == TREE_SITTER_SERIALIZATION_BUFFER_SIZE,
               "complete 16-bit indentation state must fit the native wire");

typedef struct {
    uint16_t indents[PUG_MAX_INDENTS + 1];
    uint16_t depth;
} Scanner;

static void reset_state(Scanner *scanner) { memset(scanner, 0, sizeof(*scanner)); }
static unsigned read_u16(const unsigned char *bytes) { return (unsigned)bytes[0] | (unsigned)bytes[1] << 8; }
static void write_u16(unsigned char *bytes, unsigned value) {
    bytes[0] = (unsigned char)(value & 255); bytes[1] = (unsigned char)(value >> 8);
}

static bool require_state(const Scanner *scanner) {
    if (!rootlight_pug_scope_step()) return false;
    if (!scanner) { rootlight_pug_scope_refuse(ROOTLIGHT_PUG_ALLOCATION); return false; }
    if (scanner->depth > PUG_MAX_INDENTS || scanner->indents[0] != 0) {
        rootlight_pug_scope_refuse(ROOTLIGHT_PUG_MALFORMED_STATE); return false;
    }
    for (unsigned index = 1; index <= scanner->depth; ++index) {
        if (!rootlight_pug_scope_step()) return false;
        if (scanner->indents[index] <= scanner->indents[index - 1]) {
            rootlight_pug_scope_refuse(ROOTLIGHT_PUG_MALFORMED_STATE); return false;
        }
    }
    return true;
}

static unsigned refusal_frame(char *buffer) {
    if (!buffer) { rootlight_pug_scope_refuse(ROOTLIGHT_PUG_MALFORMED_STATE); return 0; }
    buffer[0] = 'P'; buffer[1] = (char)255; buffer[2] = 0; buffer[3] = 0;
    return PUG_WIRE_HEADER;
}

void *tree_sitter_pug_external_scanner_create(void) {
    if (!rootlight_pug_scope_step()) return NULL;
    Scanner *scanner = calloc(1, sizeof(Scanner));
    if (!scanner) rootlight_pug_scope_refuse(ROOTLIGHT_PUG_ALLOCATION);
    return scanner;
}

void tree_sitter_pug_external_scanner_destroy(void *payload) { free(payload); }

unsigned tree_sitter_pug_external_scanner_serialize(void *payload, char *buffer) {
    const Scanner *scanner = payload;
    if (!buffer) { rootlight_pug_scope_refuse(ROOTLIGHT_PUG_MALFORMED_STATE); return 0; }
    if (!require_state(scanner)) return refusal_frame(buffer);
    unsigned char wire[TREE_SITTER_SERIALIZATION_BUFFER_SIZE];
    wire[0] = 'P'; wire[1] = PUG_WIRE_VERSION; write_u16(wire + 2, scanner->depth);
    for (unsigned index = 1; index <= scanner->depth; ++index) {
        if (!rootlight_pug_scope_step()) return refusal_frame(buffer);
        write_u16(wire + PUG_WIRE_HEADER + 2 * (index - 1), scanner->indents[index]);
    }
    const unsigned length = PUG_WIRE_HEADER + 2 * scanner->depth;
    memcpy(buffer, wire, length);
    return length;
}

void tree_sitter_pug_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
    Scanner *scanner = payload;
    if (!scanner) { rootlight_pug_scope_refuse(ROOTLIGHT_PUG_ALLOCATION); return; }
    reset_state(scanner);
    if (!rootlight_pug_scope_step() || length == 0) return;
    if (!buffer || length < PUG_WIRE_HEADER || length > TREE_SITTER_SERIALIZATION_BUFFER_SIZE) {
        rootlight_pug_scope_refuse(ROOTLIGHT_PUG_MALFORMED_STATE); return;
    }
    const unsigned char *bytes = (const unsigned char *)buffer;
    const unsigned depth = read_u16(bytes + 2);
    if (bytes[0] != 'P' || bytes[1] != PUG_WIRE_VERSION || depth > PUG_MAX_INDENTS
        || length != PUG_WIRE_HEADER + 2 * depth) {
        rootlight_pug_scope_refuse(ROOTLIGHT_PUG_MALFORMED_STATE); return;
    }
    Scanner next = {0}; next.depth = (uint16_t)depth;
    for (unsigned index = 1; index <= depth; ++index) {
        if (!rootlight_pug_scope_step()) return;
        next.indents[index] = (uint16_t)read_u16(bytes + PUG_WIRE_HEADER + 2 * (index - 1));
        if (next.indents[index] <= next.indents[index - 1]) {
            rootlight_pug_scope_refuse(ROOTLIGHT_PUG_MALFORMED_STATE); return;
        }
    }
    *scanner = next;
}

static bool scalar_valid(TSLexer *lexer) {
    const int32_t scalar = lexer->lookahead;
    if (scalar == 0 && !lexer->eof(lexer)) {
        rootlight_pug_scope_refuse(ROOTLIGHT_PUG_UNSUPPORTED_NUL); return false;
    }
    if (scalar < 0 || scalar > 0x10ffff || (scalar >= 0xd800 && scalar <= 0xdfff)) {
        rootlight_pug_scope_refuse(ROOTLIGHT_PUG_INVALID_SCALAR); return false;
    }
    return true;
}

static bool skip(TSLexer *lexer) {
    if (!rootlight_pug_scope_step()) return false;
    lexer->advance(lexer, true);
    return scalar_valid(lexer);
}

static bool newline(TSLexer *lexer, const bool *valid_symbols) {
    if (!valid_symbols[NEWLINE]) return false;
    if (lexer->lookahead == '\r') {
        if (!skip(lexer)) return false;
        if (lexer->lookahead != '\n') {
            rootlight_pug_scope_refuse(ROOTLIGHT_PUG_UNSUPPORTED_INDENTATION); return false;
        }
    }
    if (!skip(lexer)) return false;
    lexer->mark_end(lexer); lexer->result_symbol = NEWLINE;
    return true;
}

bool tree_sitter_pug_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
    Scanner *scanner = payload;
    if (!require_state(scanner)) return false;
    if (!lexer || !valid_symbols || !lexer->advance || !lexer->mark_end || !lexer->eof || !lexer->get_column) {
        rootlight_pug_scope_refuse(ROOTLIGHT_PUG_MALFORMED_STATE); return false;
    }
    if (!scalar_valid(lexer)) return false;
    if (lexer->lookahead == '\n' || lexer->lookahead == '\r') return newline(lexer, valid_symbols);
    lexer->mark_end(lexer);
    if (lexer->eof(lexer)) {
        if (scanner->depth && valid_symbols[DEDENT]) {
            --scanner->depth; lexer->result_symbol = DEDENT; return true;
        }
        return false;
    }
    if (lexer->get_column(lexer) != 0) return false;
    unsigned width = 0;
    while (lexer->lookahead == ' ' || lexer->lookahead == '\t') {
        unsigned step = lexer->lookahead == '\t' ? 8 : 1;
        if (width > UINT16_MAX - step) { rootlight_pug_scope_refuse(ROOTLIGHT_PUG_CAPACITY); return false; }
        width += step;
        if (!skip(lexer)) return false;
    }
    if (lexer->lookahead == '\n' || lexer->lookahead == '\r') return newline(lexer, valid_symbols);
    if (lexer->eof(lexer)) {
        if (scanner->depth && valid_symbols[DEDENT]) {
            --scanner->depth; lexer->result_symbol = DEDENT; return true;
        }
        return false;
    }
    const unsigned previous = scanner->indents[scanner->depth];
    if (width > previous && valid_symbols[INDENT]) {
        if (scanner->depth == PUG_MAX_INDENTS) { rootlight_pug_scope_refuse(ROOTLIGHT_PUG_CAPACITY); return false; }
        scanner->indents[++scanner->depth] = (uint16_t)width;
        lexer->result_symbol = INDENT; return true;
    }
    if (width < previous && valid_symbols[DEDENT]) {
        unsigned target = scanner->depth;
        while (target && scanner->indents[target] > width) {
            if (!rootlight_pug_scope_step()) return false;
            --target;
        }
        /* A valid saved stack need not describe this speculative lex branch.
         * Decline without a token or state change; the parser owns recovery. */
        if (scanner->indents[target] != width) return false;
        --scanner->depth; lexer->result_symbol = DEDENT; return true;
    }
    return false;
}
