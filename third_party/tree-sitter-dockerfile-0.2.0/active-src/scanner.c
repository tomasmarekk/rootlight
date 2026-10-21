/* Bounded derivative of camdencheek/tree-sitter-dockerfile scanner 971acdd9.
 * Preserve upstream MIT attribution; pristine sources remain separate. The wire
 * format and sticky scope prevent a recovered tree from hiding lost state. */
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "tree_sitter/parser.h"
#include "rootlight_dockerfile_status.h"

enum TokenType { HEREDOC_MARKER, HEREDOC_LINE, HEREDOC_END, HEREDOC_NL, ERROR_SENTINEL };

enum {
    ROOTLIGHT_DOCKERFILE_HEADER = 8,
    ROOTLIGHT_DOCKERFILE_RECORD = 3,
    ROOTLIGHT_DOCKERFILE_DATA = TREE_SITTER_SERIALIZATION_BUFFER_SIZE - ROOTLIGHT_DOCKERFILE_HEADER
};

/* Every queued delimiter consumes exactly its encoded wire record. No accepted
 * state can require more than the runtime's fixed serialization buffer. */
typedef struct {
    uint16_t count;
    uint16_t used;
    uint8_t in_heredoc;
    unsigned char data[ROOTLIGHT_DOCKERFILE_DATA];
} Scanner;

static void reset_state(Scanner *state) { memset(state, 0, sizeof(*state)); }

static unsigned read_u16(const unsigned char *bytes) {
    return (unsigned)bytes[0] | (unsigned)bytes[1] << 8;
}

static void write_u16(unsigned char *bytes, unsigned value) {
    bytes[0] = (unsigned char)(value & 255);
    bytes[1] = (unsigned char)(value >> 8);
}

static unsigned encode_scalar(int32_t scalar, unsigned char bytes[4]) {
    if (scalar <= 0 || scalar > 0x10ffff || (scalar >= 0xd800 && scalar <= 0xdfff)) return 0;
    if (scalar < 0x80) { bytes[0] = (unsigned char)scalar; return 1; }
    if (scalar < 0x800) {
        bytes[0] = (unsigned char)(0xc0 | (scalar >> 6));
        bytes[1] = (unsigned char)(0x80 | (scalar & 63));
        return 2;
    }
    if (scalar < 0x10000) {
        bytes[0] = (unsigned char)(0xe0 | (scalar >> 12));
        bytes[1] = (unsigned char)(0x80 | ((scalar >> 6) & 63));
        bytes[2] = (unsigned char)(0x80 | (scalar & 63));
        return 3;
    }
    bytes[0] = (unsigned char)(0xf0 | (scalar >> 18));
    bytes[1] = (unsigned char)(0x80 | ((scalar >> 12) & 63));
    bytes[2] = (unsigned char)(0x80 | ((scalar >> 6) & 63));
    bytes[3] = (unsigned char)(0x80 | (scalar & 63));
    return 4;
}

static bool valid_utf8(const unsigned char *bytes, unsigned length) {
    for (unsigned offset = 0; offset < length;) {
        unsigned first = bytes[offset++];
        if (first > 0 && first < 0x80) continue;
        unsigned continuation;
        uint32_t scalar;
        if (first >= 0xc2 && first <= 0xdf) { continuation = 1; scalar = first & 31; }
        else if (first >= 0xe0 && first <= 0xef) { continuation = 2; scalar = first & 15; }
        else if (first >= 0xf0 && first <= 0xf4) { continuation = 3; scalar = first & 7; }
        else return false;
        if (continuation > length - offset) return false;
        for (unsigned index = 0; index < continuation; ++index) {
            unsigned next = bytes[offset++];
            if ((next & 0xc0) != 0x80) return false;
            scalar = (scalar << 6) | (next & 63);
        }
        if ((continuation == 1 && scalar < 0x80) || (continuation == 2 && scalar < 0x800)
            || (continuation == 3 && scalar < 0x10000) || scalar > 0x10ffff
            || (scalar >= 0xd800 && scalar <= 0xdfff)) return false;
    }
    return true;
}

static bool state_valid(const Scanner *state) {
    if (state->used > ROOTLIGHT_DOCKERFILE_DATA || state->count > state->used / ROOTLIGHT_DOCKERFILE_RECORD
        || state->in_heredoc > 1 || (state->count == 0 && state->in_heredoc != 0)) return false;
    unsigned offset = 0;
    for (unsigned index = 0; index < state->count; ++index) {
        if (state->used - offset < ROOTLIGHT_DOCKERFILE_RECORD || state->data[offset] > 1) return false;
        unsigned length = read_u16(state->data + offset + 1);
        offset += ROOTLIGHT_DOCKERFILE_RECORD;
        if (length > state->used - offset || !valid_utf8(state->data + offset, length)) return false;
        offset += length;
    }
    return offset == state->used;
}

static bool require_state(Scanner *state) {
    if (!state) { rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_ALLOCATION); return false; }
    if (!state_valid(state)) {
        rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_MALFORMED_STATE);
        return false;
    }
    return true;
}

static unsigned refusal_frame(char *buffer) {
    if (!buffer) { rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_MALFORMED_STATE); return 0; }
    memcpy(buffer, "RLDF\xff\0\0\0", ROOTLIGHT_DOCKERFILE_HEADER);
    return ROOTLIGHT_DOCKERFILE_HEADER;
}

void *tree_sitter_dockerfile_external_scanner_create(void) {
    if (!rootlight_dockerfile_scope_poll()) return NULL;
    Scanner *state = calloc(1, sizeof(*state));
    if (!state) rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_ALLOCATION);
    return state;
}

void tree_sitter_dockerfile_external_scanner_destroy(void *payload) { free(payload); }

unsigned tree_sitter_dockerfile_external_scanner_serialize(void *payload, char *buffer) {
    Scanner *state = payload;
    if (!rootlight_dockerfile_scope_poll() || !require_state(state)) return refusal_frame(buffer);
    if (!buffer) { rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_MALFORMED_STATE); return 0; }
    memcpy(buffer, "RLDF\1", 5);
    buffer[5] = (char)state->in_heredoc;
    write_u16((unsigned char *)buffer + 6, state->count);
    memcpy(buffer + ROOTLIGHT_DOCKERFILE_HEADER, state->data, state->used);
    return ROOTLIGHT_DOCKERFILE_HEADER + state->used;
}

void tree_sitter_dockerfile_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
    if (!rootlight_dockerfile_scope_poll()) return;
    Scanner *state = payload;
    if (!state) { rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_ALLOCATION); return; }
    Scanner next;
    reset_state(&next);
    if (length == 0) { *state = next; return; }
    if (!buffer || length < ROOTLIGHT_DOCKERFILE_HEADER || length > TREE_SITTER_SERIALIZATION_BUFFER_SIZE
        || memcmp(buffer, "RLDF\1", 5) != 0) goto malformed;
    next.in_heredoc = (unsigned char)buffer[5];
    next.count = (uint16_t)read_u16((const unsigned char *)buffer + 6);
    next.used = (uint16_t)(length - ROOTLIGHT_DOCKERFILE_HEADER);
    memcpy(next.data, buffer + ROOTLIGHT_DOCKERFILE_HEADER, next.used);
    if (!state_valid(&next)) goto malformed;
    *state = next;
    return;
malformed:
    reset_state(state);
    rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_MALFORMED_STATE);
}

static bool advance(TSLexer *lexer, bool skip) {
    if (!rootlight_dockerfile_scope_poll()) return false;
    lexer->advance(lexer, skip);
    return true;
}

static bool horizontal(int32_t scalar) {
    return scalar == ' ' || scalar == '\t' || scalar == '\r' || scalar == '\v' || scalar == '\f';
}

static bool scan_marker(Scanner *state, TSLexer *lexer) {
    while (horizontal(lexer->lookahead)) if (!advance(lexer, true)) return false;
    if (lexer->lookahead != '<' || !advance(lexer, false) || lexer->lookahead != '<'
        || !advance(lexer, false)) return false;
    bool stripping = lexer->lookahead == '-';
    if (stripping && !advance(lexer, false)) return false;
    int32_t quote = lexer->lookahead == '\'' || lexer->lookahead == '"' ? lexer->lookahead : 0;
    if (quote && !advance(lexer, false)) return false;
    unsigned start = state->used;
    if (ROOTLIGHT_DOCKERFILE_DATA - start < ROOTLIGHT_DOCKERFILE_RECORD) {
        rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_CAPACITY);
        return false;
    }
    unsigned offset = start + ROOTLIGHT_DOCKERFILE_RECORD;
    while (lexer->lookahead != 0 && (quote ? lexer->lookahead != quote
        : !horizontal(lexer->lookahead) && lexer->lookahead != '\n')) {
        if (quote && (lexer->lookahead == '\n' || lexer->lookahead == '\r')) {
            rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_UNSUPPORTED_MARKER);
            return false;
        }
        if (lexer->lookahead == '\\') {
            if (quote) {
                rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_UNSUPPORTED_MARKER);
                return false;
            }
            if (!advance(lexer, false)) return false;
            if (lexer->lookahead == 0 || lexer->lookahead == '\n' || lexer->lookahead == '\r') return false;
        } else if (!quote && (lexer->lookahead == '\'' || lexer->lookahead == '"')) {
            rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_UNSUPPORTED_MARKER);
            return false;
        }
        unsigned char bytes[4];
        unsigned length = encode_scalar(lexer->lookahead, bytes);
        if (!length) { rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_INVALID_SCALAR); return false; }
        if (length > ROOTLIGHT_DOCKERFILE_DATA - offset) {
            rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_CAPACITY);
            return false;
        }
        memcpy(state->data + offset, bytes, length);
        offset += length;
        if (!advance(lexer, false)) return false;
    }
    if (lexer->lookahead == 0 && !lexer->eof(lexer)) {
        rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_UNSUPPORTED_MARKER);
        return false;
    }
    if (quote) {
        if (lexer->lookahead != quote || !advance(lexer, false)) return false;
        if (lexer->lookahead != 0 && !horizontal(lexer->lookahead) && lexer->lookahead != '\n') {
            rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_UNSUPPORTED_MARKER);
            return false;
        }
    }
    state->data[start] = stripping;
    write_u16(state->data + start + 1, offset - start - ROOTLIGHT_DOCKERFILE_RECORD);
    state->used = (uint16_t)offset;
    ++state->count;
    lexer->mark_end(lexer);
    lexer->result_symbol = HEREDOC_MARKER;
    return true;
}

static bool scan_content(Scanner *state, TSLexer *lexer, const bool *valid) {
    if (state->count == 0) return false;
    if (lexer->lookahead == 0 && lexer->eof(lexer)
        && (!valid[HEREDOC_END] || read_u16(state->data + 1) != 0)) return false;
    if (state->data[0]) while (lexer->lookahead == '\t') if (!advance(lexer, true)) return false;
    if (valid[HEREDOC_END]) {
        unsigned length = read_u16(state->data + 1);
        unsigned offset = 0;
        while (offset < length) {
            unsigned char bytes[4];
            unsigned size = encode_scalar(lexer->lookahead, bytes);
            if (!size || size > length - offset || memcmp(state->data + 3 + offset, bytes, size) != 0) break;
            offset += size;
            if (!advance(lexer, false)) return false;
        }
        if (offset == length) {
            lexer->mark_end(lexer);
            bool at_end = lexer->lookahead == '\n' || (lexer->lookahead == 0 && lexer->eof(lexer));
            if (lexer->lookahead == '\r') {
                if (!advance(lexer, false)) return false;
                at_end = lexer->lookahead == '\n';
            }
            if (at_end) {
                unsigned removed = ROOTLIGHT_DOCKERFILE_RECORD + length;
                state->used = (uint16_t)(state->used - removed);
                memmove(state->data, state->data + removed, state->used);
                memset(state->data + state->used, 0, ROOTLIGHT_DOCKERFILE_DATA - state->used);
                --state->count;
                state->in_heredoc = state->count > 0;
                lexer->result_symbol = HEREDOC_END;
                return true;
            }
        }
    }
    if (!valid[HEREDOC_LINE]) return false;
    state->in_heredoc = true;
    while (lexer->lookahead != '\n' && !(lexer->lookahead == 0 && lexer->eof(lexer))) {
        if (!advance(lexer, false)) return false;
    }
    lexer->mark_end(lexer);
    lexer->result_symbol = HEREDOC_LINE;
    return true;
}

static bool scan(Scanner *state, TSLexer *lexer, const bool *valid) {
    if (valid[ERROR_SENTINEL]) return state->in_heredoc
        ? scan_content(state, lexer, valid) : scan_marker(state, lexer);
    if (valid[HEREDOC_NL] && state->count > 0 && lexer->lookahead == '\n') {
        if (!advance(lexer, false)) return false;
        lexer->mark_end(lexer);
        lexer->result_symbol = HEREDOC_NL;
        return true;
    }
    if (valid[HEREDOC_MARKER]) return scan_marker(state, lexer);
    if (valid[HEREDOC_LINE] || valid[HEREDOC_END]) return scan_content(state, lexer, valid);
    return false;
}

bool tree_sitter_dockerfile_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
    Scanner *state = payload;
    if (!rootlight_dockerfile_scope_poll() || !require_state(state)) return false;
    if (!lexer || !valid_symbols) {
        rootlight_dockerfile_scope_refuse(ROOTLIGHT_DOCKERFILE_MALFORMED_STATE);
        return false;
    }
    Scanner next = *state;
    if (!scan(&next, lexer, valid_symbols) || !rootlight_dockerfile_scope_poll()) return false;
    if (!require_state(&next)) return false;
    *state = next;
    return true;
}
