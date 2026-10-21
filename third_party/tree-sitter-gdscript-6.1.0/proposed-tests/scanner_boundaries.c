/* Boundary proposals for the exact GDScript scanner derivative and status scope.
 * The scanner is included to inspect complete data-state conservation and inject
 * one allocation refusal. Root must compile and run this source independently. */
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef NDEBUG
#error scanner boundary assertions must remain enabled
#endif

static bool decline_allocation;
static unsigned allocation_calls;
static void *controlled_calloc(size_t count, size_t size) {
    ++allocation_calls;
    if (decline_allocation) {
        decline_allocation = false;
        return NULL;
    }
    return calloc(count, size);
}

#define calloc controlled_calloc
#include "../active-src/scanner.c"
#undef calloc

typedef struct {
    TSLexer lexer;
    const char *text;
    unsigned offset;
    unsigned end;
} Input;

static void input_advance(TSLexer *lexer, bool skip) {
    (void)skip;
    Input *input = (Input *)lexer;
    if (input->text[input->offset]) ++input->offset;
    lexer->lookahead = (unsigned char)input->text[input->offset];
}

static void input_mark_end(TSLexer *lexer) {
    Input *input = (Input *)lexer;
    input->end = input->offset;
}

static bool input_eof(const TSLexer *lexer) { return lexer->lookahead == 0; }

static Input input(const char *text) {
    Input value = {0};
    value.text = text;
    value.lexer.lookahead = (unsigned char)text[0];
    value.lexer.advance = input_advance;
    value.lexer.mark_end = input_mark_end;
    value.lexer.eof = input_eof;
    return value;
}

static Scanner *scanner(void) {
    Scanner *value = tree_sitter_gdscript_external_scanner_create();
    assert(value != NULL);
    return value;
}

static void exact_round_trip(Scanner *original) {
    char wire[TREE_SITTER_SERIALIZATION_BUFFER_SIZE];
    char second[TREE_SITTER_SERIALIZATION_BUFFER_SIZE];
    unsigned length = tree_sitter_gdscript_external_scanner_serialize(original, wire);
    assert(length >= ROOTLIGHT_GDSCRIPT_STATE_HEADER);
    Scanner *restored = scanner();
    tree_sitter_gdscript_external_scanner_deserialize(restored, wire, length);
    assert(memcmp(original, restored, sizeof(*original)) == 0);
    unsigned again = tree_sitter_gdscript_external_scanner_serialize(restored, second);
    assert(again == length && memcmp(wire, second, length) == 0);
    tree_sitter_gdscript_external_scanner_destroy(restored);
}

static void allocation_decline_is_sticky_and_null_cleanup_is_safe(void) {
    uint64_t scope = rootlight_gdscript_scope_begin();
    assert(scope != 0);
    unsigned before = allocation_calls;
    decline_allocation = true;
    assert(tree_sitter_gdscript_external_scanner_create() == NULL);
    assert(allocation_calls == before + 1);
    Input source = input("'");
    bool valid[13] = {0};
    valid[STRING_START] = true;
    assert(!tree_sitter_gdscript_external_scanner_scan(NULL, &source.lexer, valid));
    char bytes[TREE_SITTER_SERIALIZATION_BUFFER_SIZE];
    assert(tree_sitter_gdscript_external_scanner_serialize(NULL, bytes) == 4);
    assert((unsigned char)bytes[2] == 0xff);
    tree_sitter_gdscript_external_scanner_deserialize(NULL, NULL, 0);
    tree_sitter_gdscript_external_scanner_destroy(NULL);
    assert(rootlight_gdscript_scope_status(scope) == ROOTLIGHT_GDSCRIPT_ALLOCATION);
    assert(rootlight_gdscript_scope_finish(scope) == ROOTLIGHT_GDSCRIPT_ALLOCATION);
    scope = rootlight_gdscript_scope_begin();
    Scanner *next = scanner();
    assert(allocation_calls == before + 2);
    tree_sitter_gdscript_external_scanner_destroy(next);
    assert(rootlight_gdscript_scope_finish(scope) == ROOTLIGHT_GDSCRIPT_OK);
}

static void capacity_decline_preserves_exact_state_and_refusal_survives_backtracking(void) {
    uint64_t scope = rootlight_gdscript_scope_begin();
    assert(scope != 0);
    Scanner *value = scanner();
    Delimiter quote = {.flags = SingleQuote};
    for (unsigned index = 0; index < ROOTLIGHT_GDSCRIPT_STACK_CAPACITY; ++index) {
        assert(push_delimiter(value, quote));
    }
    Scanner before = *value;
    Input source = input("'");
    bool valid[13] = {0};
    valid[STRING_START] = true;
    assert(!tree_sitter_gdscript_external_scanner_scan(value, &source.lexer, valid));
    assert(memcmp(value, &before, sizeof(before)) == 0);
    assert(rootlight_gdscript_scope_status(scope) == ROOTLIGHT_GDSCRIPT_CAPACITY);
    tree_sitter_gdscript_external_scanner_deserialize(value, NULL, 0);
    assert(memcmp(value, &before, sizeof(before)) == 0);
    char refusal[TREE_SITTER_SERIALIZATION_BUFFER_SIZE];
    assert(tree_sitter_gdscript_external_scanner_serialize(value, refusal) == 4);
    assert((unsigned char)refusal[2] == 0xff);
    assert(rootlight_gdscript_scope_finish(scope) == ROOTLIGHT_GDSCRIPT_CAPACITY);
    scope = rootlight_gdscript_scope_begin();
    exact_round_trip(value);
    before = *value;
    source = input("r");
    assert(!tree_sitter_gdscript_external_scanner_scan(value, &source.lexer, valid));
    assert(memcmp(value, &before, sizeof(before)) == 0);
    assert(rootlight_gdscript_scope_status(scope) == ROOTLIGHT_GDSCRIPT_OK);
    assert(rootlight_gdscript_scope_finish(scope) == ROOTLIGHT_GDSCRIPT_OK);
    tree_sitter_gdscript_external_scanner_destroy(value);
}

static void exact_wire_maximum_round_trips_and_one_more_is_a_typed_refusal(void) {
    for (unsigned mode = 0; mode < 3; ++mode) {
        uint64_t scope = rootlight_gdscript_scope_begin();
        assert(scope != 0);
        Scanner *value = scanner();
        unsigned indents = mode == 0 ? ROOTLIGHT_GDSCRIPT_STACK_CAPACITY : mode == 1 ? 0 : 500;
        unsigned delimiters = ROOTLIGHT_GDSCRIPT_STACK_CAPACITY - indents;
        for (unsigned index = 1; index <= indents; ++index) assert(push_indent(value, index));
        Delimiter quote = {.flags = DoubleQuote | Raw};
        for (unsigned index = 0; index < delimiters; ++index) assert(push_delimiter(value, quote));
        unsigned char guarded[TREE_SITTER_SERIALIZATION_BUFFER_SIZE + 2];
        memset(guarded, 0xa5, sizeof(guarded));
        unsigned length = tree_sitter_gdscript_external_scanner_serialize(value, (char *)&guarded[1]);
        assert(length == TREE_SITTER_SERIALIZATION_BUFFER_SIZE);
        assert(guarded[0] == 0xa5 && guarded[sizeof(guarded) - 1] == 0xa5);
        exact_round_trip(value);
        Scanner before = *value;
        bool accepted = mode == 0 ? push_indent(value, indents + 1) : push_delimiter(value, quote);
        assert(!accepted);
        assert(memcmp(value, &before, sizeof(before)) == 0);
        assert(rootlight_gdscript_scope_finish(scope) == ROOTLIGHT_GDSCRIPT_CAPACITY);
        tree_sitter_gdscript_external_scanner_destroy(value);
    }
}

static void malformed_restore_resets_safely_without_partial_acceptance(void) {
    static const unsigned char cases[][16] = {
        {255},
        {'G', 'D', 1, 9, 0, 0, 0},
        {'G', 'D', 2, 0, 0, 0, 0},
        {'G', 'D', 1, 0, 0, 1, 0},
        {'G', 'D', 1, 0, 0, 1, 0, 128},
        {'G', 'D', 1, 0, 0, 1, 0, 128, 0},
        {'G', 'D', 1, 0, 0, 1, 0, 0},
        {'G', 'D', 1, 0, 0, 1, 0, 255, 255, 255, 255, 16},
        {'G', 'D', 1, 1, 0, 0, 0, 0},
        {'G', 'D', 1, 0, 0, 0, 0, 1},
    };
    static const unsigned lengths[] = {1, 7, 7, 7, 8, 9, 8, 12, 8, 8};
    for (unsigned index = 0; index < sizeof(lengths) / sizeof(lengths[0]) + 2; ++index) {
        uint64_t scope = rootlight_gdscript_scope_begin();
        assert(scope != 0);
        Scanner *value = scanner();
        assert(push_indent(value, 65536));
        const char *bytes = index < 10 ? (const char *)cases[index] : NULL;
        unsigned length = index < 10 ? lengths[index] : index == 10 ? 1 : TREE_SITTER_SERIALIZATION_BUFFER_SIZE + 1;
        tree_sitter_gdscript_external_scanner_deserialize(value, bytes, length);
        Scanner empty;
        reset_state(&empty);
        assert(memcmp(value, &empty, sizeof(empty)) == 0);
        assert(rootlight_gdscript_scope_status(scope) == ROOTLIGHT_GDSCRIPT_MALFORMED_STATE);
        assert(rootlight_gdscript_scope_finish(scope) == ROOTLIGHT_GDSCRIPT_MALFORMED_STATE);
        tree_sitter_gdscript_external_scanner_destroy(value);
    }
}

static void scope_conflicts_cannot_clear_refusal_or_steal_an_outer_owner(void) {
    uint64_t outer = rootlight_gdscript_scope_begin();
    assert(outer != 0);
    assert(rootlight_gdscript_scope_begin() == 0);
    assert(rootlight_gdscript_scope_finish(0) == ROOTLIGHT_GDSCRIPT_SCOPE_CONFLICT);
    assert(rootlight_gdscript_scope_status(outer) == ROOTLIGHT_GDSCRIPT_SCOPE_CONFLICT);
    assert(rootlight_gdscript_scope_finish(outer) == ROOTLIGHT_GDSCRIPT_SCOPE_CONFLICT);
    uint64_t next = rootlight_gdscript_scope_begin();
    assert(next != 0 && next != outer);
    assert(rootlight_gdscript_scope_status(next) == ROOTLIGHT_GDSCRIPT_OK);
    assert(rootlight_gdscript_scope_finish(outer) == ROOTLIGHT_GDSCRIPT_SCOPE_CONFLICT);
    assert(rootlight_gdscript_scope_status(next) == ROOTLIGHT_GDSCRIPT_SCOPE_CONFLICT);
    assert(rootlight_gdscript_scope_finish(next) == ROOTLIGHT_GDSCRIPT_SCOPE_CONFLICT);
    next = rootlight_gdscript_scope_begin();
    assert(next != 0);
    assert(rootlight_gdscript_scope_finish(next) == ROOTLIGHT_GDSCRIPT_OK);
}

static void unrepresentable_internal_state_never_serializes_as_truncated_success(void) {
    uint64_t scope = rootlight_gdscript_scope_begin();
    assert(scope != 0);
    Scanner *value = scanner();
    value->indents.len = ROOTLIGHT_GDSCRIPT_STACK_CAPACITY + 1;
    for (unsigned index = 1; index < value->indents.len; ++index) value->indents.data[index] = index * 1000;
    Scanner before = *value;
    char wire[TREE_SITTER_SERIALIZATION_BUFFER_SIZE];
    assert(tree_sitter_gdscript_external_scanner_serialize(value, wire) == 4);
    assert((unsigned char)wire[2] == 0xff);
    assert(memcmp(value, &before, sizeof(before)) == 0);
    assert(rootlight_gdscript_scope_finish(scope) == ROOTLIGHT_GDSCRIPT_CAPACITY);
    tree_sitter_gdscript_external_scanner_destroy(value);
}

static void wide_and_deep_representable_states_round_trip_without_truncation(void) {
    uint64_t scope = rootlight_gdscript_scope_begin();
    assert(scope != 0);
    Scanner *value = scanner();
    for (unsigned index = 1; index <= 1000; ++index) assert(push_indent(value, index));
    exact_round_trip(value);
    tree_sitter_gdscript_external_scanner_destroy(value);
    assert(rootlight_gdscript_scope_finish(scope) == ROOTLIGHT_GDSCRIPT_OK);
    scope = rootlight_gdscript_scope_begin();
    value = scanner();
    for (unsigned index = 1; index <= 300; ++index) assert(push_indent(value, index));
    assert(push_indent(value, 65535));
    assert(push_indent(value, 65536));
    assert(push_indent(value, UINT32_MAX));
    Delimiter quote = {.flags = SingleQuote | Triple | NodePath};
    for (unsigned index = 0; index < 300; ++index) assert(push_delimiter(value, quote));
    exact_round_trip(value);
    tree_sitter_gdscript_external_scanner_destroy(value);
    assert(rootlight_gdscript_scope_finish(scope) == ROOTLIGHT_GDSCRIPT_OK);
}

int main(void) {
#define RUN(name) do { name(); puts("ok " #name); } while (0)
    RUN(allocation_decline_is_sticky_and_null_cleanup_is_safe);
    RUN(capacity_decline_preserves_exact_state_and_refusal_survives_backtracking);
    RUN(exact_wire_maximum_round_trips_and_one_more_is_a_typed_refusal);
    RUN(malformed_restore_resets_safely_without_partial_acceptance);
    RUN(scope_conflicts_cannot_clear_refusal_or_steal_an_outer_owner);
    RUN(unrepresentable_internal_state_never_serializes_as_truncated_success);
    RUN(wide_and_deep_representable_states_round_trip_without_truncation);
    return 0;
}
