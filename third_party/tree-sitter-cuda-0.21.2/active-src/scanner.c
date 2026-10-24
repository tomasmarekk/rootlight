/* Bound the retained CUDA raw-string scanner to one checked native operation.
 * Candidate payloads commit only after a valid token and final scope poll;
 * refusal is outside snapshots so parser recovery cannot erase it. */
#include "tree_sitter/alloc.h"
#include "tree_sitter/parser.h"
#include "rootlight_cuda_status.h"
#include <limits.h>
#include <string.h>
#include <wctype.h>

enum TokenType { RAW_STRING_DELIMITER, RAW_STRING_CONTENT };
#define MAX_DELIMITER_LENGTH 16u
#define WIRE_HEADER 4u
#define WIRE_MAX (WIRE_HEADER + 4u * MAX_DELIMITER_LENGTH)
_Static_assert(WIRE_MAX <= TREE_SITTER_SERIALIZATION_BUFFER_SIZE, "bounded CUDA state wire");

typedef struct {
    uint32_t delimiter[MAX_DELIMITER_LENGTH];
    uint8_t delimiter_length;
} Scanner;

static bool scalar(int32_t value) {
    return value >= 0 && value <= 0x10ffff && !(value >= 0xd800 && value <= 0xdfff);
}

static bool valid(const Scanner *scanner) {
    if (!scanner || scanner->delimiter_length > MAX_DELIMITER_LENGTH) {
        rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_MALFORMED_STATE);
        return false;
    }
    for (unsigned i = 0; i < MAX_DELIMITER_LENGTH; ++i) {
        if (!rootlight_cuda_scope_step()) return false;
        uint32_t value = scanner->delimiter[i];
        if ((i < scanner->delimiter_length && (value > 0x10ffff || (value >= 0xd800 && value <= 0xdfff)))
            || (i >= scanner->delimiter_length && value != 0)) {
            rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_MALFORMED_STATE);
            return false;
        }
    }
    return true;
}

static bool advance(TSLexer *lexer) {
    if (!rootlight_cuda_scope_step()) return false;
    if (!scalar(lexer->lookahead)) {
        rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_INVALID_SCALAR);
        return false;
    }
    lexer->advance(lexer, false);
    return true;
}

static bool delimiter(Scanner *scanner, TSLexer *lexer) {
    if (scanner->delimiter_length) {
        for (unsigned i = 0; i < scanner->delimiter_length; ++i) {
            if (!rootlight_cuda_scope_step()) return false;
            if (lexer->eof(lexer) || lexer->lookahead != scanner->delimiter[i]) return false;
            if (!advance(lexer)) return false;
        }
        memset(scanner, 0, sizeof(*scanner));
        return true;
    }
    for (;;) {
        if (!rootlight_cuda_scope_step()) return false;
        if (!scalar(lexer->lookahead)) {
            rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_INVALID_SCALAR);
            return false;
        }
        if (lexer->eof(lexer) || lexer->lookahead == '\\'
            || (lexer->lookahead <= WCHAR_MAX && iswspace((wint_t)lexer->lookahead))) return false;
        /* The opening paren is not a seventeenth delimiter scalar. */
        if (lexer->lookahead == '(') return scanner->delimiter_length != 0;
        if (scanner->delimiter_length == MAX_DELIMITER_LENGTH) {
            rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_CAPACITY);
            return false;
        }
        scanner->delimiter[scanner->delimiter_length++] = (uint32_t)lexer->lookahead;
        if (!advance(lexer)) return false;
    }
}

static bool content(const Scanner *scanner, TSLexer *lexer) {
    int delimiter_index = -1;
    for (;;) {
        if (!rootlight_cuda_scope_step()) return false;
        if (lexer->eof(lexer)) {
            lexer->mark_end(lexer);
            return true;
        }
        if (!scalar(lexer->lookahead)) {
            rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_INVALID_SCALAR);
            return false;
        }
        if (delimiter_index >= 0) {
            if ((unsigned)delimiter_index == scanner->delimiter_length) {
                if (lexer->lookahead == '"') return true;
                delimiter_index = -1;
            } else if ((uint32_t)lexer->lookahead == scanner->delimiter[delimiter_index]) {
                ++delimiter_index;
            } else {
                delimiter_index = -1;
            }
        }
        if (delimiter_index == -1 && lexer->lookahead == ')') {
            lexer->mark_end(lexer);
            delimiter_index = 0;
        }
        if (!advance(lexer)) return false;
    }
}

void *tree_sitter_cuda_external_scanner_create(void) {
    if (!rootlight_cuda_scope_step()) return NULL;
    Scanner *scanner = ts_calloc(1, sizeof(Scanner));
    if (!scanner) rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_ALLOCATION);
    return scanner;
}

bool tree_sitter_cuda_external_scanner_scan(void *payload, TSLexer *lexer, const bool *symbols) {
    if (!rootlight_cuda_scope_step()) return false;
    if (!lexer || !symbols) {
        rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_MALFORMED_STATE);
        return false;
    }
    if (!valid(payload)) return false;
    Scanner next = *(Scanner *)payload;
    TSSymbol previous = lexer->result_symbol;
    bool accepted = false;
    if (!(symbols[RAW_STRING_DELIMITER] && symbols[RAW_STRING_CONTENT])) {
        if (symbols[RAW_STRING_DELIMITER]) {
            lexer->result_symbol = RAW_STRING_DELIMITER;
            accepted = delimiter(&next, lexer);
        } else if (symbols[RAW_STRING_CONTENT]) {
            lexer->result_symbol = RAW_STRING_CONTENT;
            accepted = content(&next, lexer);
        }
    }
    if (accepted && lexer->result_symbol <= RAW_STRING_CONTENT
        && symbols[lexer->result_symbol] && rootlight_cuda_scope_poll()) {
        *(Scanner *)payload = next;
        return true;
    }
    lexer->result_symbol = previous;
    return false;
}

unsigned tree_sitter_cuda_external_scanner_serialize(void *payload, char *buffer) {
    if (!rootlight_cuda_scope_step() || !valid(payload)) return 0;
    if (!buffer) {
        rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_MALFORMED_STATE);
        return 0;
    }
    Scanner *scanner = payload;
    unsigned length = WIRE_HEADER + 4u * scanner->delimiter_length;
    unsigned char staged[WIRE_MAX] = {1, scanner->delimiter_length, 0, 0};
    for (unsigned i = 0; i < scanner->delimiter_length; ++i) {
        if (!rootlight_cuda_scope_step()) return 0;
        uint32_t value = scanner->delimiter[i];
        for (unsigned byte = 0; byte < 4; ++byte) staged[WIRE_HEADER + 4u * i + byte] = (unsigned char)(value >> (8u * byte));
    }
    if (!rootlight_cuda_scope_poll()) return 0;
    memcpy(buffer, staged, length);
    return length;
}

void tree_sitter_cuda_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
    if (!rootlight_cuda_scope_step()) return;
    if (!payload) {
        rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_MALFORMED_STATE);
        return;
    }
    Scanner next = {0};
    if (length != 0) {
        if (!buffer || length < WIRE_HEADER || length > WIRE_MAX
            || (unsigned char)buffer[0] != 1 || buffer[2] || buffer[3]) goto malformed;
        next.delimiter_length = (unsigned char)buffer[1];
        if (next.delimiter_length > MAX_DELIMITER_LENGTH
            || length != WIRE_HEADER + 4u * next.delimiter_length) goto malformed;
        for (unsigned i = 0; i < next.delimiter_length; ++i) {
            if (!rootlight_cuda_scope_step()) return;
            uint32_t value = 0;
            for (unsigned byte = 0; byte < 4; ++byte) value |= (uint32_t)(unsigned char)buffer[WIRE_HEADER + 4u * i + byte] << (8u * byte);
            if (value > 0x10ffff || (value >= 0xd800 && value <= 0xdfff)) goto malformed;
            next.delimiter[i] = value;
        }
    }
    if (rootlight_cuda_scope_poll()) *(Scanner *)payload = next;
    return;
malformed:
    rootlight_cuda_scope_refuse(ROOTLIGHT_CUDA_MALFORMED_STATE);
}

void tree_sitter_cuda_external_scanner_destroy(void *payload) {
    ts_free(payload);
}
