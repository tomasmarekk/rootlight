#include <tree_sitter/parser.h>

/* Fixed and free profiles use separate native symbols, never global mode state.
 * The free profile leaves original bytes and source coordinates untouched. */
#ifndef ROOTLIGHT_COBOL_FREE_FORMAT
#define ROOTLIGHT_COBOL_FREE_FORMAT 0
#endif

enum TokenType {
    WHITE_SPACES,
    LINE_PREFIX_COMMENT,
    LINE_SUFFIX_COMMENT,
    LINE_COMMENT,
    COMMENT_ENTRY,
    multiline_string,
};

void *tree_sitter_COBOL_external_scanner_create(void) {
    return NULL;
}

static bool is_white_space(int32_t c) {
    return c == ' ' || c == '\t' || c == '\r' || c == '\n' ||
           c == '\f' || c == '\v' || c == ';' || c == ',';
}

static bool is_horizontal_separator(int32_t c) {
    return is_white_space(c) && c != '\r' && c != '\n';
}

enum { COMMENT_KEYWORD_COUNT = 9 };
static const char *const any_content_keyword[COMMENT_KEYWORD_COUNT] = {
    "author",
    "installation",
    "date-written",
    "date-compiled",
    "security",
    "identification division",
    "environment division",
    "data division",
    "procedure division",
};

static bool line_end(const TSLexer *lexer) {
    return lexer->eof(lexer) || lexer->lookahead == '\r' || lexer->lookahead == '\n';
}

static bool within_text_area(TSLexer *lexer) {
    return ROOTLIGHT_COBOL_FREE_FORMAT || lexer->get_column(lexer) < 72;
}

static int32_t ascii_lower(int32_t c) {
    return c >= 'A' && c <= 'Z' ? c + ('a' - 'A') : c;
}

static bool start_with_word(TSLexer *lexer) {
    while(lexer->lookahead == ' ' || lexer->lookahead == '\t') {
        lexer->advance(lexer, true);
    }

    const char *keyword_pointer[COMMENT_KEYWORD_COUNT];
    bool continue_check[COMMENT_KEYWORD_COUNT];
    bool word_gap[COMMENT_KEYWORD_COUNT] = {false};
    for(int i=0; i<COMMENT_KEYWORD_COUNT; ++i) {
        keyword_pointer[i] = any_content_keyword[i];
        continue_check[i] = true;
    }

    while(true) {
        for(int i=0; i<COMMENT_KEYWORD_COUNT; ++i) {
            if(continue_check[i] && *keyword_pointer[i] == 0) {
                if(!within_text_area(lexer) || line_end(lexer) || is_white_space(lexer->lookahead) || lexer->lookahead == '.') {
                    return true;
                }
                continue_check[i] = false;
            }
        }
        if(!within_text_area(lexer) || line_end(lexer)) {
            return false;
        }

        bool all_match_failed = true;
        for(int i=0; i<COMMENT_KEYWORD_COUNT; ++i) {
            if(continue_check[i]) {
                all_match_failed = false;
            }
        }

        if(all_match_failed) {
            for(; within_text_area(lexer) && !line_end(lexer);
            lexer->advance(lexer, false)) {
            }
            return false;
        }

        for(int i=0; i<COMMENT_KEYWORD_COUNT; ++i) {
            /* Failed candidates cannot advance beyond their string storage. */
            if(continue_check[i]) {
                bool horizontal_space = is_horizontal_separator(lexer->lookahead);
                if(word_gap[i] && horizontal_space) {
                    continue;
                }
                word_gap[i] = false;
                if(*keyword_pointer[i] == ' ' && horizontal_space) {
                    keyword_pointer[i]++;
                    word_gap[i] = true;
                    continue;
                }
                continue_check[i] = ascii_lower(lexer->lookahead) == *keyword_pointer[i];
                if(continue_check[i]) {
                    keyword_pointer[i]++;
                }
            }
        }

        lexer->advance(lexer, false);
    }

    return false;
}

bool tree_sitter_COBOL_external_scanner_scan(void *payload, TSLexer *lexer,
                                            const bool *valid_symbols) {
    (void)payload;
    /* Recovery offers every external token; no specific token is justified. */
    bool recovery = true;
    for(int i=0; i<6; ++i) {
        recovery = recovery && valid_symbols[i];
    }
    if(recovery || lexer->eof(lexer)) {
        return false;
    }

    if(valid_symbols[WHITE_SPACES]) {
        if(is_white_space(lexer->lookahead)) {
            while(!lexer->eof(lexer) && is_white_space(lexer->lookahead)) {
                lexer->advance(lexer, true);
            }
            lexer->result_symbol = WHITE_SPACES;
            lexer->mark_end(lexer);
            return true;
        }
    }

    if(!ROOTLIGHT_COBOL_FREE_FORMAT && valid_symbols[LINE_PREFIX_COMMENT] && lexer->get_column(lexer) <= 5) {
        if(line_end(lexer)) {
            return false;
        }
        while(lexer->get_column(lexer) <= 5 && !line_end(lexer)) {
            lexer->advance(lexer, true);
        }
        lexer->result_symbol = LINE_PREFIX_COMMENT;
        lexer->mark_end(lexer);
        return true;
    }

    if(!ROOTLIGHT_COBOL_FREE_FORMAT && valid_symbols[LINE_COMMENT]) {
        if(lexer->get_column(lexer) == 6) {
            if(lexer->lookahead == '*' || lexer->lookahead == '/') {
                while(!line_end(lexer)) {
                    lexer->advance(lexer, true);
                }
                lexer->result_symbol = LINE_COMMENT;
                lexer->mark_end(lexer);
                return true;
            } else {
                return false;
            }
        }
    }

    if(!ROOTLIGHT_COBOL_FREE_FORMAT && valid_symbols[LINE_SUFFIX_COMMENT]) {
        if(lexer->get_column(lexer) >= 72 && !line_end(lexer)) {
            while(!line_end(lexer)) {
                lexer->advance(lexer, true);
            }
            lexer->result_symbol = LINE_SUFFIX_COMMENT;
            lexer->mark_end(lexer);
            return true;
        }
    }

    if(valid_symbols[COMMENT_ENTRY]) {
        if(line_end(lexer) || !within_text_area(lexer)) {
            return false;
        }
        if(!start_with_word(lexer)) {
            lexer->mark_end(lexer);
            lexer->result_symbol = COMMENT_ENTRY;
            return true;
        } else {
            return false;
        }
    }

    if(!ROOTLIGHT_COBOL_FREE_FORMAT && valid_symbols[multiline_string]) {
        int32_t quote = lexer->lookahead;
        if(quote != '"' && quote != '\'') {
            return false;
        }
        while(true) {
            if(lexer->lookahead != quote || lexer->eof(lexer) || lexer->get_column(lexer) >= 72) {
                return false;
            }
            lexer->advance(lexer, false);
            while(true) {
                while(lexer->lookahead != quote && !line_end(lexer) && lexer->get_column(lexer) < 72) {
                    lexer->advance(lexer, false);
                }
                if(lexer->lookahead != quote || lexer->eof(lexer) || lexer->get_column(lexer) >= 72) {
                    break;
                }
                lexer->advance(lexer, false);
                /* A doubled delimiter is literal content, not authored closure. */
                if(lexer->lookahead == quote && !lexer->eof(lexer) && lexer->get_column(lexer) < 72) {
                    lexer->advance(lexer, false);
                    continue;
                }
                lexer->result_symbol = multiline_string;
                lexer->mark_end(lexer);
                return true;
            }
            /* Interior framing belongs to the complete original literal span.
             * skip=true would reset Tree-sitter's token start to its last segment. */
            while(!line_end(lexer)) {
                lexer->advance(lexer, false);
            }
            if(lexer->eof(lexer)) {
                return false;
            }
            if(lexer->lookahead == '\r') {
                lexer->advance(lexer, false);
                if(lexer->eof(lexer) || lexer->lookahead != '\n') {
                    return false;
                }
            }
            lexer->advance(lexer, false);
            int i;
            for(i=0; i<=5; ++i) {
                if(line_end(lexer)) {
                    return false;
                }
                lexer->advance(lexer, false);
            }

            if(lexer->lookahead != '-') {
                return false;
            }

            lexer->advance(lexer, false);
            while(lexer->lookahead == ' ' && lexer->get_column(lexer) < 72) {
                lexer->advance(lexer, false);
            }
        }
    }

    return false;
}

unsigned tree_sitter_COBOL_external_scanner_serialize(void *payload, char *buffer) {
    (void)payload;
    (void)buffer;
    return 0;
}

void tree_sitter_COBOL_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
    (void)payload;
    (void)buffer;
    (void)length;
}

void tree_sitter_COBOL_external_scanner_destroy(void *payload) {
    (void)payload;
}
