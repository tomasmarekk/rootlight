/* Bounded derivative of talha-akram/tree-sitter-coffeescript aaba9067, MIT.
 * Pristine sources and attribution remain separate. Complete versioned state
 * and sticky host refusal prevent recovery from hiding scanner data loss. */
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "tree_sitter/parser.h"
#include "rootlight_coffeescript_status.h"

enum TokenType { NEWLINE, INDENT, DEDENT, IMPLICIT_CALL_OPEN };
enum {
    COFFEE_WIRE_HEADER = 6,
    COFFEE_MAX_INDENTS = (TREE_SITTER_SERIALIZATION_BUFFER_SIZE - COFFEE_WIRE_HEADER) / 2,
    COFFEE_WIRE_VERSION = 1,
    COFFEE_DECLINED_VERSION = 255
};

_Static_assert(COFFEE_MAX_INDENTS >= 1 && COFFEE_MAX_INDENTS <= UINT16_MAX, "wire count range");
_Static_assert(COFFEE_WIRE_HEADER + 2 * COFFEE_MAX_INDENTS <= TREE_SITTER_SERIALIZATION_BUFFER_SIZE,
               "complete scanner state must fit the runtime buffer");

typedef struct {
    uint16_t indent_stack[COFFEE_MAX_INDENTS];
    uint16_t indent_stack_size;
    uint16_t dedent_count;
    uint8_t newline_pending;
    uint8_t indent_pending;
} Scanner;

static void reset_state(Scanner *scanner) {
    memset(scanner, 0, sizeof(*scanner));
    scanner->indent_stack_size = 1;
}

static unsigned read_u16(const unsigned char *bytes) {
    return (unsigned)bytes[0] | (unsigned)bytes[1] << 8;
}

static void write_u16(unsigned char *bytes, unsigned value) {
    bytes[0] = (unsigned char)(value & 255);
    bytes[1] = (unsigned char)(value >> 8);
}

static bool state_valid(const Scanner *scanner) {
    if (!scanner || scanner->indent_stack_size == 0 || scanner->indent_stack_size > COFFEE_MAX_INDENTS
        || scanner->indent_stack[0] != 0 || scanner->newline_pending > 1 || scanner->indent_pending > 1
        || scanner->dedent_count > COFFEE_MAX_INDENTS - scanner->indent_stack_size
        || (scanner->indent_pending && (scanner->indent_stack_size < 2 || scanner->dedent_count != 0))) return false;
    for (unsigned index = 1; index < scanner->indent_stack_size; ++index) {
        if (!rootlight_coffeescript_scope_step()) return false;
        if (scanner->indent_stack[index] <= scanner->indent_stack[index - 1]) return false;
    }
    return true;
}

static bool require_state(const Scanner *scanner) {
    if (!scanner) {
        rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_ALLOCATION);
        return false;
    }
    if (!state_valid(scanner)) {
        rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_MALFORMED_STATE);
        return false;
    }
    return true;
}

static unsigned refusal_frame(char *buffer) {
    if (!buffer) {
        rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_MALFORMED_STATE);
        return 0;
    }
    unsigned char *bytes = (unsigned char *)buffer;
    bytes[0] = COFFEE_DECLINED_VERSION;
    bytes[1] = 0;
    write_u16(bytes + 2, 1);
    write_u16(bytes + 4, 0);
    write_u16(bytes + 6, 0);
    return COFFEE_WIRE_HEADER + 2;
}

void *tree_sitter_coffeescript_external_scanner_create(void) {
    if (!rootlight_coffeescript_scope_step()) return NULL;
    Scanner *scanner = calloc(1, sizeof(*scanner));
    if (!scanner) {
        rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_ALLOCATION);
        return NULL;
    }
    reset_state(scanner);
    return scanner;
}

void tree_sitter_coffeescript_external_scanner_destroy(void *payload) { free(payload); }

void tree_sitter_coffeescript_external_scanner_reset(void *payload) {
    if (!rootlight_coffeescript_scope_step()) return;
    if (!payload) { rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_ALLOCATION); return; }
    reset_state(payload);
}

unsigned tree_sitter_coffeescript_external_scanner_serialize(void *payload, char *buffer) {
    Scanner *scanner = payload;
    if (!rootlight_coffeescript_scope_step() || !require_state(scanner)) return refusal_frame(buffer);
    if (!buffer) { rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_MALFORMED_STATE); return 0; }
    unsigned char *bytes = (unsigned char *)buffer;
    bytes[0] = COFFEE_WIRE_VERSION;
    bytes[1] = scanner->newline_pending | (scanner->indent_pending << 1);
    write_u16(bytes + 2, scanner->indent_stack_size);
    for (unsigned index = 0; index < scanner->indent_stack_size; ++index) {
        if (!rootlight_coffeescript_scope_step()) return refusal_frame(buffer);
        write_u16(bytes + 4 + index * 2, scanner->indent_stack[index]);
    }
    write_u16(bytes + 4 + scanner->indent_stack_size * 2, scanner->dedent_count);
    return COFFEE_WIRE_HEADER + scanner->indent_stack_size * 2;
}

void tree_sitter_coffeescript_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
    Scanner *scanner = payload;
    if (!rootlight_coffeescript_scope_step()) return;
    if (!scanner) { rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_ALLOCATION); return; }
    Scanner next;
    reset_state(&next);
    if (length == 0) { *scanner = next; return; }
    if (!buffer || length < COFFEE_WIRE_HEADER + 2 || length > TREE_SITTER_SERIALIZATION_BUFFER_SIZE) goto malformed;
    const unsigned char *bytes = (const unsigned char *)buffer;
    if (bytes[0] != COFFEE_WIRE_VERSION || (bytes[1] & ~3u) != 0) goto malformed;
    unsigned count = read_u16(bytes + 2);
    if (count == 0 || count > COFFEE_MAX_INDENTS || length != COFFEE_WIRE_HEADER + 2 * count) goto malformed;
    next.indent_stack_size = (uint16_t)count;
    next.newline_pending = bytes[1] & 1u;
    next.indent_pending = (bytes[1] >> 1) & 1u;
    for (unsigned index = 0; index < count; ++index) {
        if (!rootlight_coffeescript_scope_step()) return;
        next.indent_stack[index] = (uint16_t)read_u16(bytes + 4 + index * 2);
    }
    next.dedent_count = (uint16_t)read_u16(bytes + 4 + count * 2);
    if (!state_valid(&next)) goto malformed;
    *scanner = next;
    return;
malformed:
    reset_state(scanner);
    rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_MALFORMED_STATE);
}

static bool scalar_valid(TSLexer *lexer) {
    int32_t scalar = lexer->lookahead;
    if (scalar == 0 && !lexer->eof(lexer)) {
        rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_UNSUPPORTED_NUL);
        return false;
    }
    if (scalar < 0 || scalar > 0x10ffff || (scalar >= 0xd800 && scalar <= 0xdfff)) {
        rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_INVALID_SCALAR);
        return false;
    }
    return true;
}

static bool advance(TSLexer *lexer, bool skip) {
    if (!rootlight_coffeescript_scope_step() || !scalar_valid(lexer)) return false;
    lexer->advance(lexer, skip);
    return scalar_valid(lexer);
}

static bool calculate_indent(TSLexer *lexer, uint16_t *result) {
    unsigned length = 0;
    while (lexer->lookahead == ' ' || lexer->lookahead == '\t' || lexer->lookahead == '\f') {
        unsigned addition = lexer->lookahead == ' ' ? 1 : lexer->lookahead == '\t' ? 8 - length % 8 : 0;
        if (addition > UINT16_MAX - length) {
            rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_CAPACITY);
            return false;
        }
        length += addition;
        if (!advance(lexer, true)) return false;
    }
    *result = (uint16_t)length;
    return true;
}

static bool handle_newline(Scanner *scanner, TSLexer *lexer) {
    if (!advance(lexer, false)) return false;
    lexer->mark_end(lexer);
    scanner->newline_pending = 1;
    for (;;) {
        uint16_t length;
        if (!calculate_indent(lexer, &length)) return false;
        uint16_t current = scanner->indent_stack[scanner->indent_stack_size - 1];
        if (lexer->lookahead == '#') return true;
        if (lexer->lookahead == '\r') { if (!advance(lexer, true)) return false; continue; }
        if (lexer->lookahead == '\n') {
            if (!advance(lexer, false)) return false;
            lexer->mark_end(lexer);
            continue;
        }
        if (length > current) {
            if (scanner->indent_stack_size == COFFEE_MAX_INDENTS) {
                rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_CAPACITY);
                return false;
            }
            scanner->indent_stack[scanner->indent_stack_size++] = length;
            scanner->indent_pending = 1;
        } else if (length < current) {
            unsigned target = scanner->indent_stack_size;
            while (target > 1 && length < scanner->indent_stack[target - 1]) {
                if (!rootlight_coffeescript_scope_step()) return false;
                --target;
            }
            if (scanner->indent_stack[target - 1] != length) {
                rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_UNSUPPORTED_INDENTATION);
                return false;
            }
            unsigned popped = scanner->indent_stack_size - target;
            if (popped > COFFEE_MAX_INDENTS - target - scanner->dedent_count) {
                rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_CAPACITY);
                return false;
            }
            scanner->indent_stack_size = (uint16_t)target;
            scanner->dedent_count += (uint16_t)popped;
        }
        return true;
    }
}

static bool is_ident_start(int32_t scalar) {
    return (scalar >= 'a' && scalar <= 'z') || (scalar >= 'A' && scalar <= 'Z') || scalar == '_' || scalar == '$';
}

static bool is_ident_part(int32_t scalar) { return is_ident_start(scalar) || (scalar >= '0' && scalar <= '9'); }

static bool is_non_argument_word(const char *word, bool long_word) {
    static const char *const words[] = {"and", "or", "is", "isnt", "instanceof", "in", "of",
        "then", "else", "when", "by", "extends", "if", "unless", "while", "until", "for", "catch", "finally"};
    /* All excluded words fit completely. Longer identifiers are consumed under
     * credit but cannot equal this closed ASCII keyword set. */
    if (long_word) return false;
    for (unsigned index = 0; index < sizeof(words) / sizeof(words[0]); ++index) {
        if (!rootlight_coffeescript_scope_step()) return true;
        if (strcmp(word, words[index]) == 0) return true;
    }
    return false;
}

static bool scan_implicit_call_open(TSLexer *lexer) {
    lexer->mark_end(lexer);
    while (lexer->lookahead == ' ' || lexer->lookahead == '\t') if (!advance(lexer, false)) return false;
    int32_t scalar = lexer->lookahead;
    if (lexer->eof(lexer) || scalar == '\n' || scalar == '\r') return false;
    if (scalar == '"' || scalar == '\'' || scalar == '@' || scalar == '[' || scalar == '{'
        || (scalar >= '0' && scalar <= '9')) {
        lexer->result_symbol = IMPLICIT_CALL_OPEN;
        return true;
    }
    if (scalar == '-' || scalar == '+') {
        if (!advance(lexer, false)) return false;
        int32_t next = lexer->lookahead;
        if (lexer->eof(lexer) || next == ' ' || next == '\t' || next == '\n' || next == '\r'
            || next == '=' || next == scalar || (scalar == '-' && next == '>')) return false;
        lexer->result_symbol = IMPLICIT_CALL_OPEN;
        return true;
    }
    if (is_ident_start(scalar)) {
        char word[16];
        unsigned length = 0;
        bool long_word = false;
        while (is_ident_part(lexer->lookahead)) {
            if (length < sizeof(word) - 1) word[length++] = (char)lexer->lookahead;
            else long_word = true;
            if (!advance(lexer, false)) return false;
        }
        word[length] = '\0';
        if (is_non_argument_word(word, long_word)) return false;
        lexer->result_symbol = IMPLICIT_CALL_OPEN;
        return true;
    }
    return false;
}

static bool emit_pending(Scanner *scanner, TSLexer *lexer, const bool *valid) {
    if (scanner->newline_pending && valid[NEWLINE]) {
        scanner->newline_pending = 0; lexer->result_symbol = NEWLINE; return true;
    }
    if (scanner->dedent_count && valid[DEDENT]) {
        --scanner->dedent_count; lexer->result_symbol = DEDENT; return true;
    }
    if (scanner->indent_pending && valid[INDENT]) {
        scanner->indent_pending = 0; lexer->result_symbol = INDENT; return true;
    }
    return false;
}

static bool scan(Scanner *scanner, TSLexer *lexer, const bool *valid) {
    if (emit_pending(scanner, lexer, valid)) return true;
    if (scanner->newline_pending || scanner->dedent_count || scanner->indent_pending) return false;
    if (valid[IMPLICIT_CALL_OPEN] && (lexer->lookahead == ' ' || lexer->lookahead == '\t')) {
        return scan_implicit_call_open(lexer);
    }
    if (!valid[NEWLINE] && !valid[INDENT] && !valid[DEDENT]) return false;
    while (lexer->lookahead == '\r') if (!advance(lexer, true)) return false;
    if (lexer->eof(lexer)) {
        if (valid[DEDENT] && scanner->indent_stack_size > 1) {
            --scanner->indent_stack_size; lexer->result_symbol = DEDENT; return true;
        }
        return false;
    }
    if (lexer->lookahead != '\n' || !handle_newline(scanner, lexer)) return false;
    return emit_pending(scanner, lexer, valid);
}

bool tree_sitter_coffeescript_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
    Scanner *scanner = payload;
    if (!rootlight_coffeescript_scope_step() || !require_state(scanner)) return false;
    if (!lexer || !valid_symbols || !lexer->advance || !lexer->mark_end || !lexer->eof) {
        rootlight_coffeescript_scope_refuse(ROOTLIGHT_COFFEESCRIPT_MALFORMED_STATE);
        return false;
    }
    if (!scalar_valid(lexer)) return false;
    Scanner next = *scanner;
    if (!scan(&next, lexer, valid_symbols) || !rootlight_coffeescript_scope_poll() || !require_state(&next)) return false;
    *scanner = next;
    return true;
}
