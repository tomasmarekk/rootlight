/* Finite direct controls for the private CUDA scanner candidate.
 * Include the implementation only to observe exact payload transactionality
 * and inject allocation refusal; no runtime or production admission is implied. */
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static bool deny_allocation;
static void *controlled_calloc(size_t count, size_t size) {
    return deny_allocation ? NULL : calloc(count, size);
}
#define ts_calloc controlled_calloc
#include "../active-src/scanner.c"

typedef struct {
    TSLexer lexer;
    const int32_t *source;
    unsigned length, at, marked;
    bool *cancel_on_advance;
} Mock;

static void mock_advance(TSLexer *lexer, bool skip) {
    (void)skip;
    Mock *mock = (Mock *)lexer;
    if (mock->at < mock->length) ++mock->at;
    lexer->lookahead = mock->at < mock->length ? mock->source[mock->at] : 0;
    if (mock->cancel_on_advance) *mock->cancel_on_advance = true;
}
static void mock_mark(TSLexer *lexer) { ((Mock *)lexer)->marked = ((Mock *)lexer)->at; }
static bool mock_eof(const TSLexer *lexer) { return ((const Mock *)lexer)->at == ((const Mock *)lexer)->length; }
static uint32_t mock_column(TSLexer *lexer) { return ((Mock *)lexer)->at; }
static Mock mock(const int32_t *source, unsigned length) {
    Mock output = {0};
    output.source = source;
    output.length = length;
    output.lexer.advance = mock_advance;
    output.lexer.mark_end = mock_mark;
    output.lexer.eof = mock_eof;
    output.lexer.get_column = mock_column;
    output.lexer.lookahead = length ? source[0] : 0;
    output.lexer.result_symbol = 77;
    return output;
}
static unsigned assertions;
#define CONTROL(name) do { ++assertions; puts(name); } while (0)

static bool same_state(const Scanner *left, const Scanner *right) {
    return left->delimiter_length == right->delimiter_length
        && !memcmp(left->delimiter, right->delimiter, sizeof(left->delimiter));
}

static void allocation_control(void) {
    uint64_t token = rootlight_cuda_scope_begin(1000);
    deny_allocation = true;
    assert(tree_sitter_cuda_external_scanner_create() == NULL);
    deny_allocation = false;
    assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_ALLOCATION);
    CONTROL("allocation_refusal_is_typed");
}

static void wire_controls(void) {
    uint64_t token = rootlight_cuda_scope_begin(10000);
    Scanner original = {0}, restored = {0};
    original.delimiter_length = 3;
    original.delimiter[0] = 'a'; original.delimiter[1] = 0x100; original.delimiter[2] = 0x1f600;
    char buffer[TREE_SITTER_SERIALIZATION_BUFFER_SIZE]; memset(buffer, 0x5a, sizeof(buffer));
    unsigned length = tree_sitter_cuda_external_scanner_serialize(&original, buffer);
    assert(length == 16 && (unsigned char)buffer[0] == 1 && buffer[1] == 3 && !buffer[2] && !buffer[3]);
    assert((unsigned char)buffer[12] == 0 && (unsigned char)buffer[13] == 0xf6 && buffer[14] == 1 && buffer[15] == 0);
    assert((unsigned char)buffer[length] == 0x5a);
    tree_sitter_cuda_external_scanner_deserialize(&restored, buffer, length);
    assert(same_state(&original, &restored));
    assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_OK);
    CONTROL("canonical_wire_keeps_non_bmp_scalars");
    token = rootlight_cuda_scope_begin(10000);
    tree_sitter_cuda_external_scanner_deserialize(&restored, NULL, 0);
    Scanner empty = {0}; assert(same_state(&restored, &empty));
    assert(tree_sitter_cuda_external_scanner_serialize(&restored, buffer) == 4);
    assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_OK);
    CONTROL("empty_restore_is_complete_reset");

    const unsigned lengths[] = {1, 3, 5, 7, 9, 69, UINT_MAX};
    for (unsigned i = 0; i < sizeof(lengths) / sizeof(*lengths); ++i) {
        token = rootlight_cuda_scope_begin(10000);
        restored = original;
        tree_sitter_cuda_external_scanner_deserialize(&restored, buffer, lengths[i]);
        assert(same_state(&original, &restored));
        assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_MALFORMED_STATE);
    }
    CONTROL("malformed_lengths_cannot_mutate_payload");
    const unsigned char invalid[][8] = {
        {2, 0, 0, 0}, {1, 0, 1, 0}, {1, 17, 0, 0},
        {1, 1, 0, 0, 0, 0xd8, 0, 0}, {1, 1, 0, 0, 0, 0, 0x11, 0}
    };
    for (unsigned i = 0; i < sizeof(invalid) / sizeof(*invalid); ++i) {
        token = rootlight_cuda_scope_begin(10000);
        restored = original;
        unsigned size = i < 3 ? 4 : 8;
        tree_sitter_cuda_external_scanner_deserialize(&restored, (const char *)invalid[i], size);
        assert(same_state(&original, &restored));
        assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_MALFORMED_STATE);
    }
    CONTROL("version_reserved_capacity_and_invalid_scalars_are_refused");
    token = rootlight_cuda_scope_begin(10000);
    memset(buffer, 0x5a, sizeof(buffer));
    restored = original; restored.delimiter_length = 17;
    assert(tree_sitter_cuda_external_scanner_serialize(&restored, buffer) == 0);
    for (unsigned i = 0; i < sizeof(buffer); ++i) assert((unsigned char)buffer[i] == 0x5a);
    assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_MALFORMED_STATE);
    CONTROL("invalid_live_state_cannot_partially_serialize");
}

static void scan_controls(void) {
    const bool none[] = {false, false}, opening[] = {true, false}, contents[] = {false, true}, both[] = {true, true};
    const int32_t word[] = {'a', 'b', 'c', ' '};
    Scanner empty = {0}, payload = {0};
    Mock lexer = mock(word, 4);
    uint64_t token = rootlight_cuda_scope_begin(10000);
    assert(!tree_sitter_cuda_external_scanner_scan(&payload, &lexer.lexer, none));
    assert(same_state(&empty, &payload) && lexer.lexer.result_symbol == 77 && lexer.at == 0);
    assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_OK);
    CONTROL("false_symbol_mask_preserves_payload_and_result");
    token = rootlight_cuda_scope_begin(10000); lexer = mock(word, 4);
    assert(!tree_sitter_cuda_external_scanner_scan(&payload, &lexer.lexer, both));
    assert(same_state(&empty, &payload) && lexer.at == 0);
    assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_OK);
    CONTROL("recovery_symbol_mask_declines_cleanly");
    token = rootlight_cuda_scope_begin(10000); lexer = mock(word, 4);
    assert(!tree_sitter_cuda_external_scanner_scan(&payload, &lexer.lexer, opening));
    assert(same_state(&empty, &payload) && lexer.lexer.result_symbol == 77);
    assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_OK);
    CONTROL("ordinary_lexical_decline_is_transactional");
    int32_t maximum[18]; for (unsigned i = 0; i < 18; ++i) maximum[i] = 'a';
    for (unsigned size = 15; size <= 16; ++size) {
        maximum[size] = '('; payload = empty; lexer = mock(maximum, size + 1);
        token = rootlight_cuda_scope_begin(10000);
        assert(tree_sitter_cuda_external_scanner_scan(&payload, &lexer.lexer, opening));
        assert(payload.delimiter_length == size && lexer.at == size && lexer.lexer.result_symbol == RAW_STRING_DELIMITER);
        assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_OK);
        maximum[size] = 'a';
    }
    CONTROL("fifteen_and_sixteen_scalar_delimiters_fit");
    maximum[17] = '('; payload = empty; lexer = mock(maximum, 18);
    token = rootlight_cuda_scope_begin(10000);
    assert(!tree_sitter_cuda_external_scanner_scan(&payload, &lexer.lexer, opening));
    assert(same_state(&empty, &payload) && lexer.lexer.result_symbol == 77);
    assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_CAPACITY);
    CONTROL("seventeenth_scalar_refuses_capacity_without_truncation");
    payload = empty; payload.delimiter_length = 2; payload.delimiter[0] = 'a'; payload.delimiter[1] = 'b';
    Scanner before = payload; const int32_t wrong[] = {'a', 'x'}; lexer = mock(wrong, 2);
    token = rootlight_cuda_scope_begin(10000);
    assert(!tree_sitter_cuda_external_scanner_scan(&payload, &lexer.lexer, opening));
    assert(same_state(&before, &payload) && lexer.lexer.result_symbol == 77);
    assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_OK);
    CONTROL("closing_mismatch_does_not_reset_state");
    const int32_t close[] = {'a', 'b'}; lexer = mock(close, 2);
    token = rootlight_cuda_scope_begin(10000);
    assert(tree_sitter_cuda_external_scanner_scan(&payload, &lexer.lexer, opening));
    assert(same_state(&empty, &payload));
    assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_OK);
    CONTROL("matching_closing_delimiter_resets_all_state");
    payload.delimiter_length = 1; payload.delimiter[0] = 'a'; before = payload;
    const int32_t nul_content[] = {0, 'x', ')', 'a', '"'}; lexer = mock(nul_content, 5);
    token = rootlight_cuda_scope_begin(10000);
    assert(tree_sitter_cuda_external_scanner_scan(&payload, &lexer.lexer, contents));
    assert(lexer.marked == 2 && lexer.at == 4 && same_state(&before, &payload));
    assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_OK);
    CONTROL("embedded_nul_content_is_not_eof");
    token = rootlight_cuda_scope_begin(40); payload = empty; lexer = mock(word, 4);
    /* The scanner's state visits and advances exhaust this finite credit. */
    int32_t long_word[32]; for (unsigned i = 0; i < 32; ++i) long_word[i] = 'x';
    lexer = mock(long_word, 32);
    assert(!tree_sitter_cuda_external_scanner_scan(&payload, &lexer.lexer, opening));
    assert(same_state(&empty, &payload) && rootlight_cuda_scope_work(token) == 40);
    assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_WORK_LIMIT);
    CONTROL("mid_scan_work_refusal_preserves_payload");
    const int32_t invalid[] = {-1}; payload = empty; lexer = mock(invalid, 1);
    token = rootlight_cuda_scope_begin(10000);
    assert(!tree_sitter_cuda_external_scanner_scan(&payload, &lexer.lexer, opening));
    assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_INVALID_SCALAR);
    CONTROL("invalid_input_scalar_is_strict");
}

static bool cancelled(void *context) { return *(bool *)context; }
static bool reenter(void *context) { (void)context; assert(rootlight_cuda_scope_begin(1000) == 0); return false; }
static void scope_controls(void) {
    bool stop = true;
    uint64_t token = rootlight_cuda_scope_begin(0);
    assert(!rootlight_cuda_scope_set_cancel(token, cancelled, &stop));
    assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_CANCELLED);
    CONTROL("cancellation_outranks_zero_work");
    stop = false; token = rootlight_cuda_scope_begin(10000);
    assert(rootlight_cuda_scope_set_cancel(token, cancelled, &stop));
    Scanner payload = {0}, before = {0}; const int32_t source[] = {'a', '('}; Mock lexer = mock(source, 2);
    lexer.cancel_on_advance = &stop; const bool opening[] = {true, false};
    assert(!tree_sitter_cuda_external_scanner_scan(&payload, &lexer.lexer, opening));
    assert(same_state(&before, &payload) && lexer.lexer.result_symbol == 77);
    assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_CANCELLED);
    CONTROL("mid_advance_cancellation_prevents_commit");
    token = rootlight_cuda_scope_begin(10000);
    assert(!rootlight_cuda_scope_set_cancel(token, reenter, NULL));
    assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_SCOPE_CONFLICT);
    CONTROL("reentrant_scope_cannot_replace_owner");
    token = rootlight_cuda_scope_begin(10000);
    void *owned = tree_sitter_cuda_external_scanner_create();
    assert(owned != NULL);
    tree_sitter_cuda_external_scanner_destroy(owned);
    assert(rootlight_cuda_scope_finish(token) == ROOTLIGHT_CUDA_OK);
    CONTROL("next_clean_scope_is_available");
}

int main(void) {
    allocation_control(); wire_controls(); scan_controls(); scope_controls();
    printf("controls_passed=%u\n", assertions);
    return 0;
}
