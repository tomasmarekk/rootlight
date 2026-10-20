// Copied from tree-sitter-rust with modification.

#include "tree_sitter/parser.h"

enum TokenType {
    BLOCK_COMMENT_CONTENT
};

void * tree_sitter_wat_external_scanner_create() {}
void tree_sitter_wat_external_scanner_destroy(void *payload) {}
unsigned tree_sitter_wat_external_scanner_serialize(void *payload, char *buffer) {
    return 1;
}
void tree_sitter_wat_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {}

typedef enum {
    LeftParen,
    LeftSemi,
    Continuing,
} BlockCommentState;

typedef struct {
    BlockCommentState state;
    unsigned nestingDepth;
} BlockCommentProcessing;

static inline void process_left_paren(BlockCommentProcessing *processing, char current) {
    if (current == ';') {
        processing->nestingDepth += 1;
    }
    processing->state = Continuing;
};

static inline void process_left_semi(BlockCommentProcessing *processing, char current, TSLexer *lexer) {
    if (current == ';') {
        lexer->mark_end(lexer);
        processing->state = LeftSemi;
        return;
    }

    if (current == ')') {
        processing->nestingDepth -= 1;
    }

    processing->state = Continuing;
}

static inline void process_continuing(BlockCommentProcessing *processing, char current) {
    switch (current) {
        case '(':
            processing->state = LeftParen;
            break;
        case ';':
            processing->state = LeftSemi;
            break;
    }
}

static inline bool process_block_comment(TSLexer *lexer, const bool *valid_symbols) {
    char first = (char)lexer->lookahead;
    lexer->advance(lexer, false);

    if (valid_symbols[BLOCK_COMMENT_CONTENT]) {
        BlockCommentProcessing processing = {Continuing, 1};
        switch (first) {
            case ';':
                processing.state = LeftSemi;
                if (lexer->lookahead == ')') {
                    return false;
                }
                break;
            case '(':
                processing.state = LeftParen;
                break;
            default:
                processing.state = Continuing;
                break;
        }

        while (!lexer->eof(lexer) && processing.nestingDepth != 0) {
            first = (char)lexer->lookahead;
            switch (processing.state) {
                case LeftParen:
                    process_left_paren(&processing, first);
                    break;
                case LeftSemi:
                    process_left_semi(&processing, first, lexer);
                    break;
                case Continuing:
                    lexer->mark_end(lexer);
                    process_continuing(&processing, first);
                    break;
                default:
                    break;
            }
            lexer->advance(lexer, false);
            if (first == ')' && processing.nestingDepth != 0) {
                lexer->mark_end(lexer);
            }
        }
        lexer->result_symbol = BLOCK_COMMENT_CONTENT;
        return true;
    }

    return false;
}

bool tree_sitter_wat_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
    if (valid_symbols[BLOCK_COMMENT_CONTENT]) {
        return process_block_comment(lexer, valid_symbols);
    }

    return false;
}
