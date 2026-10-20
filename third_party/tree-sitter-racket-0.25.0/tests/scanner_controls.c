/* Exercise the real external scanner with Tree-sitter-compatible lexer callbacks.
 * Codepoint lookahead and UTF-8 byte positions remain separate so delimiter
 * comparisons cannot pass by truncating encoded Unicode or consuming a prefix.
 */
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <tree_sitter/parser.h>

static size_t outstanding_allocations;

static void *scanner_calloc(size_t count, size_t width) {
    void *result = calloc(count, width);
    if (result != NULL) outstanding_allocations++;
    return result;
}

static void *scanner_realloc(void *previous, size_t bytes) {
    void *result = realloc(previous, bytes);
    if (result != NULL && previous == NULL) outstanding_allocations++;
    return result;
}

static void scanner_free(void *allocation) {
    if (allocation != NULL) {
        if (outstanding_allocations == 0) abort();
        outstanding_allocations--;
    }
    free(allocation);
}

#define calloc scanner_calloc
#define realloc scanner_realloc
#define free scanner_free
#include "../src/scanner.c"
#undef calloc
#undef realloc
#undef free

typedef struct {
    TSLexer lexer;
    const unsigned char *input;
    size_t length;
    size_t position;
    size_t advance_count;
    size_t mark_position;
    uint32_t column;
} Lexer;

static size_t decode(const Lexer *lexer, int32_t *value) {
    if (lexer->position == lexer->length) {
        *value = 0;
        return 0;
    }
    unsigned char first = lexer->input[lexer->position];
    size_t width = first < 0x80 ? 1 : first < 0xe0 ? 2 : first < 0xf0 ? 3 : 4;
    if (width > lexer->length - lexer->position) abort();
    uint32_t scalar = first & (width == 1 ? 0x7f : width == 2 ? 0x1f : width == 3 ? 0x0f : 0x07);
    for (size_t index = 1; index < width; index++) {
        unsigned char next = lexer->input[lexer->position + index];
        if ((next & 0xc0) != 0x80) abort();
        scalar = (scalar << 6) | (next & 0x3f);
    }
    *value = (int32_t)scalar;
    return width;
}

static void lexer_advance(TSLexer *raw, bool skip) {
    Lexer *lexer = (Lexer *)raw;
    (void)skip;
    if (lexer->advance_count++ > lexer->length) abort();
    int32_t scalar;
    size_t width = decode(lexer, &scalar);
    lexer->position += width;
    lexer->column = scalar == '\n' ? 0 : lexer->column + 1;
    (void)decode(lexer, &lexer->lexer.lookahead);
}

static void lexer_mark_end(TSLexer *raw) {
    Lexer *lexer = (Lexer *)raw;
    lexer->mark_position = lexer->position;
}

static uint32_t lexer_column(TSLexer *raw) {
    return ((Lexer *)raw)->column;
}

static bool lexer_range_start(const TSLexer *raw) {
    return ((const Lexer *)raw)->position == 0;
}

static bool lexer_eof(const TSLexer *raw) {
    const Lexer *lexer = (const Lexer *)raw;
    return lexer->position == lexer->length;
}

static void lexer_log(const TSLexer *raw, const char *format, ...) {
    (void)raw;
    (void)format;
}

static Lexer new_lexer(const char *input) {
    Lexer lexer = {
        .lexer = {
            .result_symbol = UINT16_MAX,
            .advance = lexer_advance,
            .mark_end = lexer_mark_end,
            .get_column = lexer_column,
            .is_at_included_range_start = lexer_range_start,
            .eof = lexer_eof,
            .log = lexer_log,
        },
        .input = (const unsigned char *)input,
        .length = strlen(input),
    };
    (void)decode(&lexer, &lexer.lexer.lookahead);
    return lexer;
}

typedef struct {
    const char *name;
    const char *input;
    bool expected_result;
    const char *expected_consumed;
} ScanCase;

static bool run_case(const ScanCase *test, void *payload) {
    Lexer lexer = new_lexer(test->input);
    const bool symbols[] = {true};
    bool result = tree_sitter_racket_external_scanner_scan(payload, &lexer.lexer, symbols);
    bool passed = result == test->expected_result && outstanding_allocations == 0;
    if (result && test->expected_result) {
        passed = passed && lexer.position == strlen(test->expected_consumed)
            && lexer.lexer.result_symbol == HERE_STRING_BODY;
    }
    printf("%s %s result=%d consumed=%zu expected=%zu live_allocations=%zu\n",
           passed ? "PASS" : "FAIL", test->name, result, lexer.position,
           test->expected_consumed == NULL ? 0 : strlen(test->expected_consumed),
           outstanding_allocations);
    return passed;
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "--verify-check-failure") == 0) {
        fputs("intentional harness check failure\n", stderr);
        return 1;
    }
    if (argc != 1) return 2;
    const ScanCase cases[] = {
        {"different-first-character-control", "END\nBODY\nEND", true, "END\nBODY\nEND"},
        {"same-first-character-regression", "END\nEXTRA\nEND", true, "END\nEXTRA\nEND"},
        {"same-prefix-longer-line", "END\nENDING\nEND", true, "END\nENDING\nEND"},
        {"same-length-different-line", "END\nEAX\nEND", true, "END\nEAX\nEND"},
        {"same-first-character-without-terminator", "END\nEXTRA", false, NULL},
        {"missing-delimiter-newline", "END", false, NULL},
        {"missing-terminator", "END\nBODY", false, NULL},
        {"empty-body-at-eof", "END\nEND", true, "END\nEND"},
        {"leaves-following-input", "END\nBODY\nEND\nTAIL", true, "END\nBODY\nEND"},
        {"unicode-same-first-scalar", u8"\U0001f642END\n\U0001f642EXTRA\n\U0001f642END", true,
         u8"\U0001f642END\n\U0001f642EXTRA\n\U0001f642END"},
        {"unicode-internal-zero-byte", u8"\u0100END\n\u0200EXTRA\n\u0100END", true,
         u8"\u0100END\n\u0200EXTRA\n\u0100END"},
        {"unicode-written-content", u8"END\n\u03bb\U0001f642\nEND", true,
         u8"END\n\u03bb\U0001f642\nEND"},
        {"growth-and-line-reuse", "END_LONG_DELIMITER_FOR_GROWTH\nEND_LONG_DELIMITER_FOR_GROWTX\nshort\nEND_LONG_DELIMITER_FOR_GROWTH", true,
         "END_LONG_DELIMITER_FOR_GROWTH\nEND_LONG_DELIMITER_FOR_GROWTX\nshort\nEND_LONG_DELIMITER_FOR_GROWTH"},
        {"empty-delimiter", "\nBODY\n\nTAIL", true, "\nBODY\n"},
        {"carriage-return-part-of-delimiter", "END\r\nBODY\nEND\r", true, "END\r\nBODY\nEND\r"},
    };
    size_t checks = 0;
    size_t failures = 0;
    void *payload = tree_sitter_racket_external_scanner_create();
    for (size_t index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
        checks++;
        if (!run_case(&cases[index], payload)) failures++;
    }

    char serialized[TREE_SITTER_SERIALIZATION_BUFFER_SIZE];
    memset(serialized, 0x5a, sizeof(serialized));
    unsigned length = tree_sitter_racket_external_scanner_serialize(payload, serialized);
    bool untouched = length == 0;
    for (size_t index = 0; index < sizeof(serialized); index++) {
        untouched = untouched && (unsigned char)serialized[index] == 0x5a;
    }
    checks++;
    failures += !untouched;
    printf("%s stateless-serialization length=%u\n", untouched ? "PASS" : "FAIL", length);
    tree_sitter_racket_external_scanner_deserialize(payload, NULL, 0);
    tree_sitter_racket_external_scanner_deserialize(payload, serialized, sizeof(serialized));
    checks++;
    if (!run_case(&cases[1], payload)) failures++;

    Lexer disabled = new_lexer("END\nEXTRA\nEND");
    const bool invalid_symbols[] = {false};
    bool invalid = tree_sitter_racket_external_scanner_scan(payload, &disabled.lexer, invalid_symbols);
    bool unchanged = !invalid && disabled.position == 0 && disabled.advance_count == 0
        && disabled.lexer.result_symbol == UINT16_MAX && outstanding_allocations == 0;
    checks++;
    failures += !unchanged;
    printf("%s invalid-symbol-preserves-callback-state\n", unchanged ? "PASS" : "FAIL");

    tree_sitter_racket_external_scanner_destroy(payload);
    printf("checks=%zu passed=%zu failed=%zu\n", checks, checks - failures, failures);
    return failures == 0 ? 0 : 1;
}
