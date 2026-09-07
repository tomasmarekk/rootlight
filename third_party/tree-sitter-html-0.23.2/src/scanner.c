#include "tag.h"
#include "tree_sitter/parser.h"

enum TokenType {
    START_TAG_NAME,
    SCRIPT_START_TAG_NAME,
    STYLE_START_TAG_NAME,
    END_TAG_NAME,
    ERRONEOUS_END_TAG_NAME,
    SELF_CLOSING_TAG_DELIMITER,
    IMPLICIT_END_TAG,
    RAW_TEXT,
    COMMENT,
    TEXT_START_TAG_NAME,
    PLAINTEXT_START_TAG_NAME,
};

#include "state.h"

static inline void advance(TSLexer *lexer) { lexer->advance(lexer, false); }
static inline void skip(TSLexer *lexer) { lexer->advance(lexer, true); }

#include "name.h"

static bool scan_comment(TSLexer *lexer) {
    if (lexer->lookahead != '-') {
        return false;
    }
    advance(lexer);
    if (lexer->lookahead != '-') {
        return false;
    }
    advance(lexer);

    unsigned dashes = 0;
    while (lexer->lookahead) {
        switch (lexer->lookahead) {
            case '-':
                if (dashes < 2) ++dashes;
                break;
            case '>':
                if (dashes >= 2) {
                    lexer->result_symbol = COMMENT;
                    advance(lexer);
                    lexer->mark_end(lexer);
                    return true;
                }
            default:
                dashes = 0;
        }
        advance(lexer);
    }
    return false;
}

static bool custom_tag_equals(const Tag *tag, const char *name) {
    size_t length = strlen(name);
    return tag->type == CUSTOM && tag->custom_tag_name.size == length &&
           memcmp(tag->custom_tag_name.contents, name, length) == 0;
}

static const char *text_end_delimiter(const Tag *tag) {
    switch (tag->type) {
        case SCRIPT: return "</SCRIPT";
        case STYLE: return "</STYLE";
        case TITLE: return "</TITLE";
        case TEXTAREA: return "</TEXTAREA";
        case IFRAME: return "</IFRAME";
        default:
            if (custom_tag_equals(tag, "XMP")) return "</XMP";
            if (custom_tag_equals(tag, "NOEMBED")) return "</NOEMBED";
            if (custom_tag_equals(tag, "NOFRAMES")) return "</NOFRAMES";
            return NULL;
    }
}

static bool in_foreign_scope(const Scanner *scanner) {
    // This source grammar does not infer namespace integration from attributes.
    // In particular, an SVG title must not select HTML RCDATA tokenization.
    for (unsigned i = 0; i < scanner->tags.size; i++) {
        TagType type = scanner->tags.contents[i].type;
        if (type == SVG || type == MATH) return true;
    }
    return false;
}

static bool scan_raw_text(Scanner *scanner, TSLexer *lexer) {
    if (scanner->tags.size == 0) {
        return false;
    }

    lexer->mark_end(lexer);

    const Tag *tag = array_back(&scanner->tags);
    if (custom_tag_equals(tag, "PLAINTEXT")) {
        while (!lexer->eof(lexer)) advance(lexer);
        lexer->mark_end(lexer);
        lexer->result_symbol = RAW_TEXT;
        return true;
    }
    const char *end_delimiter = text_end_delimiter(tag);
    if (!end_delimiter) return false;
    size_t delimiter_length = strlen(end_delimiter);

    unsigned delimiter_index = 0;
    while (!lexer->eof(lexer)) {
        if (ascii_upper(lexer->lookahead) == end_delimiter[delimiter_index]) {
            if (delimiter_index == 0) lexer->mark_end(lexer);
            delimiter_index++;
            advance(lexer);
            if (delimiter_index == delimiter_length) {
                if (html_space(lexer->lookahead) || lexer->lookahead == '/' || lexer->lookahead == '>') {
                    lexer->result_symbol = RAW_TEXT;
                    return true;
                }
                delimiter_index = 0;
                lexer->mark_end(lexer);
            }
        } else if (delimiter_index && lexer->lookahead == '<') {
            lexer->mark_end(lexer);
            delimiter_index = 1;
            advance(lexer);
        } else {
            delimiter_index = 0;
            advance(lexer);
            lexer->mark_end(lexer);
        }
    }

    lexer->mark_end(lexer);
    lexer->result_symbol = RAW_TEXT;
    return true;
}

static void pop_tag(Scanner *scanner) {
    Tag popped_tag = array_pop(&scanner->tags);
    scanner->serialized_bytes -= tag_size(&popped_tag);
    tag_free(&popped_tag);
}

static bool scan_implicit_end_tag(Scanner *scanner, TSLexer *lexer) {
    Tag *parent = scanner->tags.size == 0 ? NULL : array_back(&scanner->tags);

    bool is_closing_tag = false;
    if (lexer->lookahead == '/') {
        is_closing_tag = true;
        advance(lexer);
    } else {
        if (parent && tag_is_void(parent)) {
            pop_tag(scanner);
            lexer->result_symbol = IMPLICIT_END_TAG;
            return true;
        }
    }

    String tag_name = scan_tag_name(lexer);
    if (tag_name.size == 0 && !lexer->eof(lexer)) {
        array_delete(&tag_name);
        return false;
    }

    Tag next_tag = tag_for_name(tag_name);

    if (is_closing_tag) {
        // The tag correctly closes the topmost element on the stack
        if (scanner->tags.size > 0 && tag_eq(array_back(&scanner->tags), &next_tag)) {
            tag_free(&next_tag);
            return false;
        }

        // Otherwise, dig deeper and queue implicit end tags (to be nice in
        // the case of malformed HTML)
        for (unsigned i = scanner->tags.size; i > 0; i--) {
            if (tag_eq(&scanner->tags.contents[i - 1], &next_tag)) {
                pop_tag(scanner);
                lexer->result_symbol = IMPLICIT_END_TAG;
                tag_free(&next_tag);
                return true;
            }
        }
    } else if (
        parent &&
        (
            !tag_can_contain(parent, &next_tag) ||
            ((parent->type == HTML || parent->type == HEAD || parent->type == BODY) && lexer->eof(lexer))
        )
    ) {
        pop_tag(scanner);
        lexer->result_symbol = IMPLICIT_END_TAG;
        tag_free(&next_tag);
        return true;
    }

    tag_free(&next_tag);
    return false;
}

static bool scan_start_tag_name(Scanner *scanner, TSLexer *lexer) {
    String tag_name = scan_tag_name(lexer);
    if (tag_name.size == 0) {
        array_delete(&tag_name);
        return false;
    }

    Tag tag = tag_for_name(tag_name);
    unsigned bytes = tag_size(&tag);
    if (bytes > TREE_SITTER_SERIALIZATION_BUFFER_SIZE - scanner->serialized_bytes) {
        tag_free(&tag);
        return false;
    }
    bool plaintext = custom_tag_equals(&tag, "PLAINTEXT");
    bool text_mode = tag.type != SCRIPT && tag.type != STYLE &&
                     (plaintext || text_end_delimiter(&tag) != NULL);
    bool html_context = text_mode && !in_foreign_scope(scanner);
    array_push(&scanner->tags, tag);
    scanner->serialized_bytes += bytes;
    if (html_context && plaintext) {
        lexer->result_symbol = PLAINTEXT_START_TAG_NAME;
        return true;
    }
    if (html_context) {
        lexer->result_symbol = TEXT_START_TAG_NAME;
        return true;
    }
    switch (tag.type) {
        case SCRIPT:
            lexer->result_symbol = SCRIPT_START_TAG_NAME;
            break;
        case STYLE:
            lexer->result_symbol = STYLE_START_TAG_NAME;
            break;
        default:
            lexer->result_symbol = START_TAG_NAME;
            break;
    }
    return true;
}

static bool scan_end_tag_name(Scanner *scanner, TSLexer *lexer) {
    String tag_name = scan_tag_name(lexer);

    if (tag_name.size == 0) {
        array_delete(&tag_name);
        return false;
    }

    Tag tag = tag_for_name(tag_name);
    if (scanner->tags.size > 0 && tag_eq(array_back(&scanner->tags), &tag)) {
        pop_tag(scanner);
        lexer->result_symbol = END_TAG_NAME;
    } else {
        lexer->result_symbol = ERRONEOUS_END_TAG_NAME;
    }

    tag_free(&tag);
    return true;
}

static bool scan_self_closing_tag_delimiter(Scanner *scanner, TSLexer *lexer) {
    advance(lexer);
    if (lexer->lookahead == '>') {
        advance(lexer);
        if (scanner->tags.size > 0) {
            pop_tag(scanner);
            lexer->result_symbol = SELF_CLOSING_TAG_DELIMITER;
        }
        return true;
    }
    return false;
}

static bool scan(Scanner *scanner, TSLexer *lexer, const bool *valid_symbols) {
    if (valid_symbols[RAW_TEXT] && !valid_symbols[START_TAG_NAME] && !valid_symbols[END_TAG_NAME]) {
        return scan_raw_text(scanner, lexer);
    }

    while (html_space(lexer->lookahead)) {
        skip(lexer);
    }

    switch (lexer->lookahead) {
        case '<':
            lexer->mark_end(lexer);
            advance(lexer);

            if (lexer->lookahead == '!') {
                advance(lexer);
                return scan_comment(lexer);
            }

            if (valid_symbols[IMPLICIT_END_TAG]) {
                return scan_implicit_end_tag(scanner, lexer);
            }
            break;

        case '\0':
            if (valid_symbols[IMPLICIT_END_TAG]) {
                return scan_implicit_end_tag(scanner, lexer);
            }
            break;

        case '/':
            if (valid_symbols[SELF_CLOSING_TAG_DELIMITER]) {
                return scan_self_closing_tag_delimiter(scanner, lexer);
            }
            break;

        default:
            if ((valid_symbols[START_TAG_NAME] || valid_symbols[END_TAG_NAME]) && !valid_symbols[RAW_TEXT]) {
                return valid_symbols[START_TAG_NAME] ? scan_start_tag_name(scanner, lexer)
                                                     : scan_end_tag_name(scanner, lexer);
            }
    }

    return false;
}

void *tree_sitter_html_external_scanner_create() {
    Scanner *scanner = (Scanner *)ts_calloc(1, sizeof(Scanner));
    if (scanner) scanner->serialized_bytes = HTML_STATE_HEADER;
    return scanner;
}

bool tree_sitter_html_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
    Scanner *scanner = (Scanner *)payload;
    return scan(scanner, lexer, valid_symbols);
}

unsigned tree_sitter_html_external_scanner_serialize(void *payload, char *buffer) {
    Scanner *scanner = (Scanner *)payload;
    return serialize(scanner, buffer);
}

void tree_sitter_html_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
    Scanner *scanner = (Scanner *)payload;
    deserialize(scanner, buffer, length);
}

void tree_sitter_html_external_scanner_destroy(void *payload) {
    Scanner *scanner = (Scanner *)payload;
    for (unsigned i = 0; i < scanner->tags.size; i++) {
        tag_free(&scanner->tags.contents[i]);
    }
    array_delete(&scanner->tags);
    ts_free(scanner);
}
