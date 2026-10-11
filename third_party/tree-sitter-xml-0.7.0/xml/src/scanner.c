/* Native XML scanning retains written names and delimiter ranges without tag state.
 * Exact start/end QName comparison belongs to source-bound adapter lowering, not this lexer.
 */
#define TS_XML
#include "../../common/scanner.h"

static bool scan_tag_name(TSLexer *lexer, enum TokenType symbol) {
    if (lexer->eof(lexer) || !is_valid_name_start_char(lexer->lookahead)) return false;
    do {
        advance(lexer);
    } while (!lexer->eof(lexer) && is_valid_name_char(lexer->lookahead));
    lexer->mark_end(lexer);
    lexer->result_symbol = symbol;
    return true;
}

static inline bool in_error_recovery(const bool *valid_symbols) {
    return valid_symbols[PI_TARGET] && valid_symbols[PI_CONTENT] && valid_symbols[COMMENT] &&
           valid_symbols[CHAR_DATA] && valid_symbols[CDATA];
}

static bool scan_text(TSLexer *lexer, enum TokenType symbol) {
    if (lexer->eof(lexer)) return false;
    if (lexer->lookahead == ']') {
        advance(lexer);
        lexer->mark_end(lexer);
        if (lexer->lookahead == ']') {
            advance(lexer);
            if (lexer->lookahead == '>') return false;
        }
        /* One bracket per token keeps overlapping ]]> suffixes exact without rewind or storage. */
        lexer->result_symbol = symbol;
        return true;
    }

    bool has_content = false;
    while (!lexer->eof(lexer) && lexer->lookahead != ']') {
        if (symbol == CHAR_DATA && (lexer->lookahead == '<' || lexer->lookahead == '&')) break;
        if (!is_valid_xml_char(lexer->lookahead)) return false;
        advance(lexer);
        lexer->mark_end(lexer);
        has_content = true;
    }
    lexer->result_symbol = symbol;
    return has_content;
}

/** Consume one enabled written XML token from the caller-owned lexer.
 * The valid-symbol array has the original eleven-token order; no payload is read or mutated.
 * A true return commits result_symbol and mark_end. Tag spelling equality is not checked here.
 */
bool tree_sitter_xml_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
    (void)payload;
    if (in_error_recovery(valid_symbols)) return false;
    if (valid_symbols[PI_TARGET] || valid_symbols[XML_MODEL] || valid_symbols[XML_STYLESHEET]) {
        return scan_pi_target(lexer, valid_symbols);
    }
    if (valid_symbols[PI_CONTENT]) return scan_pi_content(lexer);
    if (valid_symbols[CDATA]) return scan_text(lexer, CDATA);
    if (valid_symbols[CHAR_DATA] && lexer->lookahead != '<' && lexer->lookahead != '&') {
        return scan_text(lexer, CHAR_DATA);
    }
    if (valid_symbols[COMMENT] && lexer->lookahead == '<') {
        advance(lexer);
        advance_if_eq(lexer, '!');
        return scan_comment(lexer);
    }
    if (valid_symbols[SELF_CLOSING_TAG_DELIMITER] && lexer->lookahead == '/') {
        advance(lexer);
        advance_if_eq(lexer, '>');
        lexer->mark_end(lexer);
        lexer->result_symbol = SELF_CLOSING_TAG_DELIMITER;
        return true;
    }
    if (valid_symbols[START_TAG_NAME]) return scan_tag_name(lexer, START_TAG_NAME);
    if (valid_symbols[END_TAG_NAME]) return scan_tag_name(lexer, END_TAG_NAME);
    return false;
}

/** Return the null payload of this allocation-free scanner; there is no owned state. */
void *tree_sitter_xml_external_scanner_create(void) { return NULL; }

/** Release no resources; the scanner owns neither payload nor lexer storage. */
void tree_sitter_xml_external_scanner_destroy(void *payload) { (void)payload; }

/** Return the empty canonical state without accessing caller payload or buffer bytes. */
unsigned tree_sitter_xml_external_scanner_serialize(void *payload, char *buffer) {
    (void)payload;
    (void)buffer;
    return 0;
}

/** Accept every prior byte sequence as empty state without reading it or allocating. */
void tree_sitter_xml_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
    (void)payload;
    (void)buffer;
    (void)length;
}
