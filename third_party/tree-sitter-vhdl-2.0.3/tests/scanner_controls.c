/* Native callback regressions for the reviewed VHDL scanner boundary.
 * Including the same scanner source permits direct overflow and restoration
 * controls without pretending that arbitrary masks are real parser states. */
#include <limits.h>
#include <stdio.h>
#include <string.h>

#include "../src/scanner.c"

/* The scanner's upstream debug header shadows assert. These checks are
 * defined after every upstream include and always evaluate their condition. */
static void scanner_check_failure(const char* expression, int line)
{
    fprintf(stderr, "VHDL scanner control failed at line %d: %s\n", line, expression);
    exit(EXIT_FAILURE);
}

#define ROOTLIGHT_SCANNER_CHECK(condition) do { \
    if(!(condition)) scanner_check_failure(#condition, __LINE__); \
} while(0)

typedef struct {
    TSLexer lexer;
    const unsigned char* text;
    size_t length;
    size_t position;
    size_t marked_end;
} MockLexer;

static void mock_advance(TSLexer* lexer, bool skip)
{
    (void)skip;
    MockLexer* mock = (MockLexer*)lexer;
    if(mock->position < mock->length) mock->position++;
    lexer->lookahead = mock->position < mock->length ? mock->text[mock->position] : 0;
}

static void mock_mark_end(TSLexer* lexer)
{
    MockLexer* mock = (MockLexer*)lexer;
    mock->marked_end = mock->position;
}

static uint32_t mock_column(TSLexer* lexer)
{
    MockLexer* mock = (MockLexer*)lexer;
    uint32_t column = 0;
    for(size_t index = 0; index < mock->position; index++){
        column = mock->text[index] == '\n' ? 0 : column + 1;
    }
    return column;
}

static bool mock_eof(const TSLexer* lexer)
{
    const MockLexer* mock = (const MockLexer*)lexer;
    return mock->position >= mock->length;
}

static bool mock_range_start(const TSLexer* lexer)
{
    return ((const MockLexer*)lexer)->position == 0;
}

static MockLexer mock_lexer(const char* text)
{
    MockLexer mock = {0};
    mock.text = (const unsigned char*)text;
    mock.length = strlen(text);
    mock.lexer.lookahead = mock.length ? mock.text[0] : 0;
    mock.lexer.advance = mock_advance;
    mock.lexer.mark_end = mock_mark_end;
    mock.lexer.get_column = mock_column;
    mock.lexer.eof = mock_eof;
    mock.lexer.is_at_included_range_start = mock_range_start;
    return mock;
}

static bool same_scanner(const Scanner* left, const Scanner* right)
{
    return left->is_in_directive == right->is_in_directive &&
           left->is_block_comment == right->is_block_comment &&
           left->bit_string_base == right->bit_string_base && left->base == right->base;
}

static void numeric_accumulator_controls(void)
{
    const char* exact[] = {"0#", "16#", "2_147_483_647#"};
    const int values[] = {0, 16, 2147483647};
    for(size_t index = 0; index < sizeof(exact) / sizeof(exact[0]); index++){
        MockLexer lexer = mock_lexer(exact[index]);
        ROOTLIGHT_SCANNER_CHECK(parse_integer(&lexer.lexer) == values[index]);
        ROOTLIGHT_SCANNER_CHECK(lexer.lexer.lookahead == '#');
    }
    const char* overflow[] = {"2147483648#", "999999999999999999999999999999999#"};
    for(size_t index = 0; index < sizeof(overflow) / sizeof(overflow[0]); index++){
        MockLexer lexer = mock_lexer(overflow[index]);
        ROOTLIGHT_SCANNER_CHECK(parse_integer(&lexer.lexer) == INT_MAX);
        ROOTLIGHT_SCANNER_CHECK(lexer.lexer.lookahead == '#');
    }
    char long_digits[8194];
    memset(long_digits, '9', 8192);
    long_digits[8192] = '#';
    long_digits[8193] = 0;
    MockLexer lexer = mock_lexer(long_digits);
    ROOTLIGHT_SCANNER_CHECK(parse_integer(&lexer.lexer) == INT_MAX);
    ROOTLIGHT_SCANNER_CHECK(lexer.position == 8192 && lexer.lexer.lookahead == '#');
    bool mask[ERROR_SENTINEL + 1] = {false};
    mask[TOKEN_DECIMAL_FLOAT] = true;
    const char* fractions[] = {"1.2147483648", "1.999999999999999999999999999999e999999999999999999999"};
    for(size_t index = 0; index < sizeof(fractions) / sizeof(fractions[0]); index++){
        Scanner scanner = {0};
        lexer = mock_lexer(fractions[index]);
        ROOTLIGHT_SCANNER_CHECK(tree_sitter_vhdl_external_scanner_scan(&scanner, &lexer.lexer, mask));
        ROOTLIGHT_SCANNER_CHECK(lexer.lexer.result_symbol == TOKEN_DECIMAL_FLOAT && mock_eof(&lexer.lexer));
    }
}

static void restore_controls(void)
{
    const Scanner dirty = {true, true, BASE_SPECIFIER_HEX, 16};
    const Scanner empty = {0};
    Scanner scanner = dirty;
    char encoded[SCANNER_STATE_BYTES];
    char second[SCANNER_STATE_BYTES];
    ROOTLIGHT_SCANNER_CHECK(tree_sitter_vhdl_external_scanner_serialize(&scanner, encoded) == SCANNER_STATE_BYTES);
    ROOTLIGHT_SCANNER_CHECK(tree_sitter_vhdl_external_scanner_serialize(&scanner, second) == SCANNER_STATE_BYTES);
    ROOTLIGHT_SCANNER_CHECK(memcmp(encoded, second, sizeof(encoded)) == 0);
    const unsigned char expected[] = {1, 3, 4, 16, 0, 0, 0};
    ROOTLIGHT_SCANNER_CHECK(memcmp(encoded, expected, sizeof(expected)) == 0);
    scanner = empty;
    tree_sitter_vhdl_external_scanner_deserialize(&scanner, encoded, sizeof(encoded));
    ROOTLIGHT_SCANNER_CHECK(same_scanner(&scanner, &dirty));
    scanner = dirty;
    tree_sitter_vhdl_external_scanner_deserialize(&scanner, NULL, 0);
    ROOTLIGHT_SCANNER_CHECK(same_scanner(&scanner, &empty));
    for(unsigned length = 0; length < SCANNER_STATE_BYTES; length++){
        scanner = dirty;
        tree_sitter_vhdl_external_scanner_deserialize(&scanner, encoded, length);
        ROOTLIGHT_SCANNER_CHECK(same_scanner(&scanner, &empty));
    }
    scanner = dirty;
    tree_sitter_vhdl_external_scanner_deserialize(&scanner, encoded, SCANNER_STATE_BYTES + 1);
    ROOTLIGHT_SCANNER_CHECK(same_scanner(&scanner, &empty));
    for(unsigned index = 0; index < 3; index++){
        memcpy(second, encoded, sizeof(second));
        second[index] = index == 0 ? 2 : index == 1 ? 4 : 5;
        scanner = dirty;
        tree_sitter_vhdl_external_scanner_deserialize(&scanner, second, sizeof(second));
        ROOTLIGHT_SCANNER_CHECK(same_scanner(&scanner, &empty));
    }
    memcpy(second, encoded, sizeof(second));
    memset(second + 3, 255, 4);
    scanner = dirty;
    tree_sitter_vhdl_external_scanner_deserialize(&scanner, second, sizeof(second));
    ROOTLIGHT_SCANNER_CHECK(same_scanner(&scanner, &empty));
    scanner = dirty;
    scanner.base = -1;
    ROOTLIGHT_SCANNER_CHECK(tree_sitter_vhdl_external_scanner_serialize(&scanner, second) == 0);
    scanner = dirty;
    scanner.bit_string_base = ERROR_SENTINEL;
    ROOTLIGHT_SCANNER_CHECK(tree_sitter_vhdl_external_scanner_serialize(&scanner, second) == 0);
}

static void emitted_mask_controls(void)
{
    const char* inputs[] = {"1", "1.5", "16#F#", "name", "signal", "b\"101\"",
                           "\"text\"", "'Q'", "'1'", "\\Extended\\", "-- comment\n",
                           "/* comment */", "`warning body\n", "#F#", "\"101\"", ""};
    const Scanner initial[] = {{0}, {true, false, BASE_SPECIFIER_BINARY, 16},
                              {false, true, BASE_SPECIFIER_HEX, 2}};
    for(size_t sample = 0; sample < sizeof(inputs) / sizeof(inputs[0]); sample++){
        for(size_t state = 0; state < sizeof(initial) / sizeof(initial[0]); state++){
            for(unsigned token = 0; token <= ERROR_SENTINEL; token++){
                bool mask[ERROR_SENTINEL + 1] = {false};
                mask[token] = true;
                Scanner scanner = initial[state];
                MockLexer lexer = mock_lexer(inputs[sample]);
                if(tree_sitter_vhdl_external_scanner_scan(&scanner, &lexer.lexer, mask)){
                    ROOTLIGHT_SCANNER_CHECK(lexer.lexer.result_symbol < ERROR_SENTINEL);
                    ROOTLIGHT_SCANNER_CHECK(mask[lexer.lexer.result_symbol]);
                }else{
                    ROOTLIGHT_SCANNER_CHECK(same_scanner(&scanner, &initial[state]));
                }
            }
        }
    }
    bool only_float[ERROR_SENTINEL + 1] = {false};
    only_float[TOKEN_DECIMAL_FLOAT] = true;
    Scanner scanner = initial[1];
    MockLexer lexer = mock_lexer("1");
    ROOTLIGHT_SCANNER_CHECK(!tree_sitter_vhdl_external_scanner_scan(&scanner, &lexer.lexer, only_float));
    ROOTLIGHT_SCANNER_CHECK(same_scanner(&scanner, &initial[1]));
    bool integer[ERROR_SENTINEL + 1] = {false};
    integer[TOKEN_DECIMAL_INTEGER] = true;
    scanner = initial[0];
    lexer = mock_lexer("1");
    ROOTLIGHT_SCANNER_CHECK(tree_sitter_vhdl_external_scanner_scan(&scanner, &lexer.lexer, integer));
    ROOTLIGHT_SCANNER_CHECK(lexer.lexer.result_symbol == TOKEN_DECIMAL_INTEGER);
}

int main(int argc, char** argv)
{
    if(argc == 2 && strcmp(argv[1], "--verify-check-failure") == 0){
        ROOTLIGHT_SCANNER_CHECK(false);
        return EXIT_SUCCESS;
    }
    if(argc != 1) return EXIT_FAILURE;
    numeric_accumulator_controls();
    restore_controls();
    emitted_mask_controls();
    puts("VHDL scanner callback controls passed");
    return 0;
}
