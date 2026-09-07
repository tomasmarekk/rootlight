#include "tree_sitter/alloc.h"
#include "tree_sitter/parser.h"
#include <limits.h>

#define _str(x) #x
#define _file(x) _str(schema.x.c)

#ifndef YAML_SCHEMA
#define YAML_SCHEMA core
#endif

#include _file(YAML_SCHEMA)

// clang-format off

typedef enum {
    END_OF_FILE,

    S_DIR_YML_BGN,  R_DIR_YML_VER,
    S_DIR_TAG_BGN,  R_DIR_TAG_HDL,  R_DIR_TAG_PFX,
    S_DIR_RSV_BGN,  R_DIR_RSV_PRM,
    S_DRS_END,
    S_DOC_END,
    R_BLK_SEQ_BGN,  BR_BLK_SEQ_BGN, B_BLK_SEQ_BGN,
    R_BLK_KEY_BGN,  BR_BLK_KEY_BGN, B_BLK_KEY_BGN,
    R_BLK_VAL_BGN,  BR_BLK_VAL_BGN, B_BLK_VAL_BGN,
    R_BLK_IMP_BGN,
    R_BLK_LIT_BGN,  BR_BLK_LIT_BGN,
    R_BLK_FLD_BGN,  BR_BLK_FLD_BGN,
    BR_BLK_STR_CTN,
    R_FLW_SEQ_BGN,  BR_FLW_SEQ_BGN, B_FLW_SEQ_BGN,
    R_FLW_SEQ_END,  BR_FLW_SEQ_END, B_FLW_SEQ_END,
    R_FLW_MAP_BGN,  BR_FLW_MAP_BGN, B_FLW_MAP_BGN,
    R_FLW_MAP_END,  BR_FLW_MAP_END, B_FLW_MAP_END,
    R_FLW_SEP_BGN,  BR_FLW_SEP_BGN,
    R_FLW_KEY_BGN,  BR_FLW_KEY_BGN,
    R_FLW_JSV_BGN,  BR_FLW_JSV_BGN,
    R_FLW_NJV_BGN,  BR_FLW_NJV_BGN,
    R_DQT_STR_BGN,  BR_DQT_STR_BGN, B_DQT_STR_BGN,
    R_DQT_STR_CTN,  BR_DQT_STR_CTN,
    R_DQT_ESC_NWL,  BR_DQT_ESC_NWL,
    R_DQT_ESC_SEQ,  BR_DQT_ESC_SEQ,
    R_DQT_STR_END,  BR_DQT_STR_END,
    R_SQT_STR_BGN,  BR_SQT_STR_BGN, B_SQT_STR_BGN,
    R_SQT_STR_CTN,  BR_SQT_STR_CTN,
    R_SQT_ESC_SQT,  BR_SQT_ESC_SQT,
    R_SQT_STR_END,  BR_SQT_STR_END,

    R_SGL_PLN_NUL_BLK, BR_SGL_PLN_NUL_BLK, B_SGL_PLN_NUL_BLK, R_SGL_PLN_NUL_FLW, BR_SGL_PLN_NUL_FLW,
    R_SGL_PLN_BOL_BLK, BR_SGL_PLN_BOL_BLK, B_SGL_PLN_BOL_BLK, R_SGL_PLN_BOL_FLW, BR_SGL_PLN_BOL_FLW,
    R_SGL_PLN_INT_BLK, BR_SGL_PLN_INT_BLK, B_SGL_PLN_INT_BLK, R_SGL_PLN_INT_FLW, BR_SGL_PLN_INT_FLW,
    R_SGL_PLN_FLT_BLK, BR_SGL_PLN_FLT_BLK, B_SGL_PLN_FLT_BLK, R_SGL_PLN_FLT_FLW, BR_SGL_PLN_FLT_FLW,
    R_SGL_PLN_TMS_BLK, BR_SGL_PLN_TMS_BLK, B_SGL_PLN_TMS_BLK, R_SGL_PLN_TMS_FLW, BR_SGL_PLN_TMS_FLW,
    R_SGL_PLN_STR_BLK, BR_SGL_PLN_STR_BLK, B_SGL_PLN_STR_BLK, R_SGL_PLN_STR_FLW, BR_SGL_PLN_STR_FLW,

    R_MTL_PLN_STR_BLK,  BR_MTL_PLN_STR_BLK,
    R_MTL_PLN_STR_FLW,  BR_MTL_PLN_STR_FLW,

    R_TAG,     BR_TAG,     B_TAG,
    R_ACR_BGN, BR_ACR_BGN, B_ACR_BGN, R_ACR_CTN,
    R_ALS_BGN, BR_ALS_BGN, B_ALS_BGN, R_ALS_CTN,

    BL,
    COMMENT,

    ERR_REC,
} TokenType;

// clang-format on

#define SCN_SUCC 1
#define SCN_STOP 0
#define SCN_FAIL (-1)

#define IND_ROT 'r'
#define IND_MAP 'm'
#define IND_SEQ 'q'
#define IND_STR 's'

#define RET_SYM(RESULT_SYMBOL)                                                                                         \
    {                                                                                                                  \
        if (scanner->failed) return false;                                                                              \
        flush(scanner);                                                                                                \
        lexer->result_symbol = RESULT_SYMBOL;                                                                          \
        return true;                                                                                                   \
    }

#define POP_IND()                                                                                                      \
    {                                                                                                                  \
        /* incorrect status caused by error recovering */                                                              \
        if (scanner->indent_count == 1) {                                                                               \
            return false;                                                                                              \
        }                                                                                                              \
        pop_ind(scanner);                                                                                              \
    }

#define PUSH_IND(TYP, LEN) do { if (!push_ind(scanner, TYP, LEN)) return false; } while (0)

#define PUSH_BGN_IND(TYP)                                                                                              \
    {                                                                                                                  \
        if (has_tab_ind)                                                                                               \
            return false;                                                                                              \
        PUSH_IND(TYP, bgn_col);                                                                                        \
    }

#define MAY_PUSH_IMP_IND(TYP)                                                                                          \
    {                                                                                                                  \
        if (cur_ind != scanner->blk_imp_col) {                                                                         \
            if (scanner->blk_imp_tab)                                                                                  \
                return false;                                                                                          \
            PUSH_IND(IND_MAP, scanner->blk_imp_col);                                                                   \
        }                                                                                                              \
    }

#define MAY_PUSH_SPC_SEQ_IND()                                                                                         \
    {                                                                                                                  \
        if (cur_ind_typ == IND_MAP) {                                                                                  \
            PUSH_IND(IND_SEQ, bgn_col);                                                                                \
        }                                                                                                              \
    }

#define MAY_UPD_IMP_COL()                                                                                              \
    {                                                                                                                  \
        if (scanner->blk_imp_row != bgn_row) {                                                                         \
            scanner->blk_imp_row = bgn_row;                                                                            \
            scanner->blk_imp_col = bgn_col;                                                                            \
            scanner->blk_imp_tab = has_tab_ind;                                                                        \
        }                                                                                                              \
    }

#if HAS_TIMESTAMP
#define SGL_PLN_SYM(POS, CTX)                                                                                          \
    (scanner->rlt_sch == RS_NULL        ? POS##_SGL_PLN_NUL_##CTX                                                      \
     : scanner->rlt_sch == RS_BOOL      ? POS##_SGL_PLN_BOL_##CTX                                                      \
     : scanner->rlt_sch == RS_INT       ? POS##_SGL_PLN_INT_##CTX                                                      \
     : scanner->rlt_sch == RS_FLOAT     ? POS##_SGL_PLN_FLT_##CTX                                                      \
     : scanner->rlt_sch == RS_TIMESTAMP ? POS##_SGL_PLN_TMS_##CTX                                                      \
                                        : POS##_SGL_PLN_STR_##CTX)
#else
#define SGL_PLN_SYM(POS, CTX)                                                                                          \
    (scanner->rlt_sch == RS_NULL        ? POS##_SGL_PLN_NUL_##CTX                                                      \
     : scanner->rlt_sch == RS_BOOL      ? POS##_SGL_PLN_BOL_##CTX                                                      \
     : scanner->rlt_sch == RS_INT       ? POS##_SGL_PLN_INT_##CTX                                                      \
     : scanner->rlt_sch == RS_FLOAT     ? POS##_SGL_PLN_FLT_##CTX                                                      \
                                        : POS##_SGL_PLN_STR_##CTX)
#endif

// Every admitted frame fits the runtime's fixed state buffer; never serialize
// only a stack prefix, which would give incremental parsing a different context.
enum {
    STATE_HEADER_SIZE = 20,
    STATE_FRAME_SIZE = 5,
    MAX_INDENTS = (TREE_SITTER_SERIALIZATION_BUFFER_SIZE - STATE_HEADER_SIZE) / STATE_FRAME_SIZE,
};

typedef struct {
    int32_t row;
    int32_t col;
    int32_t blk_imp_row;
    int32_t blk_imp_col;
    bool blk_imp_tab;
    uint16_t indent_count;
    uint8_t indent_types[MAX_INDENTS + 1];
    int32_t indent_lengths[MAX_INDENTS + 1];
    bool failed;

    // Temporary lookahead positions are committed only by a successful token.
    int32_t end_row;
    int32_t end_col;
    int32_t cur_row;
    int32_t cur_col;
    int32_t cur_chr;
    int8_t sch_stt;
    ResultSchema rlt_sch;
} Scanner;

static void reset(Scanner *scanner) {
    scanner->row = 0;
    scanner->col = 0;
    scanner->blk_imp_row = -1;
    scanner->blk_imp_col = -1;
    scanner->blk_imp_tab = 0;
    scanner->indent_count = 1;
    scanner->indent_types[0] = IND_ROT;
    scanner->indent_lengths[0] = -1;
    scanner->failed = false;
}

static void write_position(char *buffer, int32_t position) {
    // Conversion of the -1 sentinel to unsigned is defined modulo 2^32.
    uint32_t value = (uint32_t)position;
    for (unsigned i = 0; i < 4; i++) {
        buffer[i] = (char)(value & 0xff);
        value >>= 8;
    }
}

static bool read_position(const char *buffer, bool allow_sentinel, int32_t *position) {
    uint32_t value = 0;
    for (unsigned i = 0; i < 4; i++) value |= (uint32_t)(uint8_t)buffer[i] << (8 * i);
    if (value <= INT32_MAX) {
        *position = (int32_t)value;
        return true;
    }
    if (allow_sentinel && value == UINT32_MAX) {
        *position = -1;
        return true;
    }
    return false;
}

static unsigned serialize(Scanner *scanner, char *buffer) {
    if (scanner->failed) {
        buffer[0] = (char)0xff;
        return 1;
    }
    buffer[0] = 1;
    write_position(buffer + 1, scanner->row);
    write_position(buffer + 5, scanner->col);
    write_position(buffer + 9, scanner->blk_imp_row);
    write_position(buffer + 13, scanner->blk_imp_col);
    buffer[17] = scanner->blk_imp_tab;
    unsigned count = scanner->indent_count - 1;
    buffer[18] = (char)(count & 0xff);
    buffer[19] = (char)(count >> 8);
    unsigned size = STATE_HEADER_SIZE;
    for (unsigned i = 1; i < scanner->indent_count; i++) {
        buffer[size++] = (char)scanner->indent_types[i];
        write_position(buffer + size, scanner->indent_lengths[i]);
        size += 4;
    }
    return size;
}

static void deserialize(Scanner *scanner, const char *buffer, unsigned length) {
    reset(scanner);
    if (length == 1 && buffer && (uint8_t)buffer[0] == 0xff) {
        scanner->failed = true;
        return;
    }
    if (!buffer || length < STATE_HEADER_SIZE || length > TREE_SITTER_SERIALIZATION_BUFFER_SIZE) return;
    unsigned count = (uint8_t)buffer[18] | (unsigned)(uint8_t)buffer[19] << 8;
    if (buffer[0] != 1 || (uint8_t)buffer[17] > 1 || count > MAX_INDENTS ||
        length != STATE_HEADER_SIZE + STATE_FRAME_SIZE * count) return;
    if (!read_position(buffer + 1, false, &scanner->row) ||
        !read_position(buffer + 5, false, &scanner->col) ||
        !read_position(buffer + 9, true, &scanner->blk_imp_row) ||
        !read_position(buffer + 13, true, &scanner->blk_imp_col)) goto invalid;
    for (unsigned i = 0; i < count; i++) {
        unsigned offset = STATE_HEADER_SIZE + STATE_FRAME_SIZE * i;
        uint8_t type = (uint8_t)buffer[offset];
        if ((type != IND_MAP && type != IND_SEQ && type != IND_STR) ||
            !read_position(buffer + offset + 1, type == IND_STR, &scanner->indent_lengths[i + 1])) goto invalid;
        scanner->indent_types[i + 1] = type;
    }
    scanner->blk_imp_tab = buffer[17] != 0;
    scanner->indent_count = (uint16_t)(count + 1);
    return;
invalid:
    reset(scanner);
}

static inline void increment(Scanner *scanner, int32_t *position) {
    if (*position == INT32_MAX) scanner->failed = true;
    else (*position)++;
}

static inline void adv(Scanner *scanner, TSLexer *lexer) {
    increment(scanner, &scanner->cur_col);
    scanner->cur_chr = lexer->lookahead;
    lexer->advance(lexer, false);
}

static inline void adv_nwl(Scanner *scanner, TSLexer *lexer) {
    increment(scanner, &scanner->cur_row);
    scanner->cur_col = 0;
    scanner->cur_chr = lexer->lookahead;
    lexer->advance(lexer, false);
}

static inline void skp(Scanner *scanner, TSLexer *lexer) {
    increment(scanner, &scanner->cur_col);
    scanner->cur_chr = lexer->lookahead;
    lexer->advance(lexer, true);
}

static inline void skp_nwl(Scanner *scanner, TSLexer *lexer) {
    increment(scanner, &scanner->cur_row);
    scanner->cur_col = 0;
    scanner->cur_chr = lexer->lookahead;
    lexer->advance(lexer, true);
}

static inline void mrk_end(Scanner *scanner, TSLexer *lexer) {
    scanner->end_row = scanner->cur_row;
    scanner->end_col = scanner->cur_col;
    lexer->mark_end(lexer);
}

static inline void init(Scanner *scanner) {
    scanner->cur_row = scanner->row;
    scanner->cur_col = scanner->col;
    scanner->cur_chr = 0;
    scanner->sch_stt = 0;
    scanner->rlt_sch = RS_STR;
}

static inline void flush(Scanner *scanner) {
    scanner->row = scanner->end_row;
    scanner->col = scanner->end_col;
}

static inline void pop_ind(Scanner *scanner) {
    scanner->indent_count--;
}

static inline bool push_ind(Scanner *scanner, uint8_t typ, int32_t len) {
    if (scanner->indent_count == MAX_INDENTS + 1) {
        scanner->failed = true;
        return false;
    }
    scanner->indent_types[scanner->indent_count] = typ;
    scanner->indent_lengths[scanner->indent_count++] = len;
    return true;
}

static inline bool is_wsp(int32_t c) { return c == ' ' || c == '\t'; }

static inline bool is_nwl(int32_t c) { return c == '\r' || c == '\n'; }

static inline bool is_wht(int32_t c) { return is_wsp(c) || is_nwl(c) || c == 0; }

static inline bool is_ns_dec_digit(int32_t c) { return c >= '0' && c <= '9'; }

static inline bool is_ns_hex_digit(int32_t c) {
    return is_ns_dec_digit(c) || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F');
}

static inline bool is_ns_word_char(int32_t c) {
    return c == '-' || (c >= '0' && c <= '9') || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z');
}

static inline bool is_nb_json(int32_t c) { return c == 0x09 || (c >= 0x20 && c <= 0x10ffff); }

static inline bool is_nb_double_char(int32_t c) { return is_nb_json(c) && c != '\\' && c != '"'; }

static inline bool is_nb_single_char(int32_t c) { return is_nb_json(c) && c != '\''; }

static inline bool is_ns_char(int32_t c) {
    return (c >= 0x21 && c <= 0x7e) || c == 0x85 || (c >= 0xa0 && c <= 0xd7ff) || (c >= 0xe000 && c <= 0xfefe) ||
           (c >= 0xff00 && c <= 0xfffd) || (c >= 0x10000 && c <= 0x10ffff);
}

static inline bool is_c_indicator(int32_t c) {
    return c == '-' || c == '?' || c == ':' || c == ',' || c == '[' || c == ']' || c == '{' || c == '}' || c == '#' ||
           c == '&' || c == '*' || c == '!' || c == '|' || c == '>' || c == '\'' || c == '"' || c == '%' || c == '@' ||
           c == '`';
}

static inline bool is_c_flow_indicator(int32_t c) { return c == ',' || c == '[' || c == ']' || c == '{' || c == '}'; }

static inline bool is_plain_safe_in_block(int32_t c) { return is_ns_char(c); }

static inline bool is_plain_safe_in_flow(int32_t c) { return is_ns_char(c) && !is_c_flow_indicator(c); }

static inline bool is_ns_uri_char(int32_t c) {
    return is_ns_word_char(c) || c == '#' || c == ';' || c == '/' || c == '?' || c == ':' || c == '@' || c == '&' ||
           c == '=' || c == '+' || c == '$' || c == ',' || c == '_' || c == '.' || c == '!' || c == '~' || c == '*' ||
           c == '\'' || c == '(' || c == ')' || c == '[' || c == ']';
}

static inline bool is_ns_tag_char(int32_t c) {
    return is_ns_word_char(c) || c == '#' || c == ';' || c == '/' || c == '?' || c == ':' || c == '@' || c == '&' ||
           c == '=' || c == '+' || c == '$' || c == '_' || c == '.' || c == '~' || c == '*' || c == '\'' || c == '(' ||
           c == ')';
}

static inline bool is_ns_anchor_char(int32_t c) { return is_ns_char(c) && !is_c_flow_indicator(c); }

static char scn_uri_esc(Scanner *scanner, TSLexer *lexer) {
    if (lexer->lookahead != '%') {
        return SCN_STOP;
    }
    mrk_end(scanner, lexer);
    adv(scanner, lexer);
    if (!is_ns_hex_digit(lexer->lookahead)) {
        return SCN_FAIL;
    }
    adv(scanner, lexer);
    if (!is_ns_hex_digit(lexer->lookahead)) {
        return SCN_FAIL;
    }
    adv(scanner, lexer);
    return SCN_SUCC;
}

static char scn_ns_uri_char(Scanner *scanner, TSLexer *lexer) {
    if (is_ns_uri_char(lexer->lookahead)) {
        adv(scanner, lexer);
        return SCN_SUCC;
    }
    return scn_uri_esc(scanner, lexer);
}

static char scn_ns_tag_char(Scanner *scanner, TSLexer *lexer) {
    if (is_ns_tag_char(lexer->lookahead)) {
        adv(scanner, lexer);
        return SCN_SUCC;
    }
    return scn_uri_esc(scanner, lexer);
}

static bool scn_dir_bgn(Scanner *scanner, TSLexer *lexer) {
    adv(scanner, lexer);
    if (lexer->lookahead == 'Y') {
        adv(scanner, lexer);
        if (lexer->lookahead == 'A') {
            adv(scanner, lexer);
            if (lexer->lookahead == 'M') {
                adv(scanner, lexer);
                if (lexer->lookahead == 'L') {
                    adv(scanner, lexer);
                    if (is_wht(lexer->lookahead)) {
                        mrk_end(scanner, lexer);
                        RET_SYM(S_DIR_YML_BGN);
                    }
                }
            }
        }
    } else if (lexer->lookahead == 'T') {
        adv(scanner, lexer);
        if (lexer->lookahead == 'A') {
            adv(scanner, lexer);
            if (lexer->lookahead == 'G') {
                adv(scanner, lexer);
                if (is_wht(lexer->lookahead)) {
                    mrk_end(scanner, lexer);
                    RET_SYM(S_DIR_TAG_BGN);
                }
            }
        }
    }
    for (;;) {
        if (!is_ns_char(lexer->lookahead)) {
            break;
        }
        adv(scanner, lexer);
    }
    if (scanner->cur_col > 1 && is_wht(lexer->lookahead)) {
        mrk_end(scanner, lexer);
        RET_SYM(S_DIR_RSV_BGN);
    }
    return false;
}

static bool scn_dir_yml_ver(Scanner *scanner, TSLexer *lexer, TSSymbol result_symbol) {
    bool has_major = false;
    bool has_minor = false;
    while (is_ns_dec_digit(lexer->lookahead)) {
        adv(scanner, lexer);
        has_major = true;
    }
    if (lexer->lookahead != '.') {
        return false;
    }
    adv(scanner, lexer);
    while (is_ns_dec_digit(lexer->lookahead)) {
        adv(scanner, lexer);
        has_minor = true;
    }
    if (!has_major || !has_minor) {
        return false;
    }
    mrk_end(scanner, lexer);
    RET_SYM(result_symbol);
}

static bool scn_tag_hdl_tal(Scanner *scanner, TSLexer *lexer) {
    if (lexer->lookahead == '!') {
        adv(scanner, lexer);
        return true;
    }
    bool has_name = false;
    while (is_ns_word_char(lexer->lookahead)) {
        adv(scanner, lexer);
        has_name = true;
    }
    if (!has_name) {
        return true;
    }
    if (lexer->lookahead == '!') {
        adv(scanner, lexer);
        return true;
    }
    return false;
}

static bool scn_dir_tag_hdl(Scanner *scanner, TSLexer *lexer, TSSymbol result_symbol) {
    if (lexer->lookahead == '!') {
        adv(scanner, lexer);
        if (scn_tag_hdl_tal(scanner, lexer)) {
            mrk_end(scanner, lexer);
            RET_SYM(result_symbol);
        }
    }
    return false;
}

static bool scn_dir_tag_pfx(Scanner *scanner, TSLexer *lexer, TSSymbol result_symbol) {
    if (lexer->lookahead == '!') {
        adv(scanner, lexer);
    } else if (scn_ns_tag_char(scanner, lexer) == SCN_SUCC) {
        ;
    } else {
        return false;
    }
    for (;;) {
        switch (scn_ns_uri_char(scanner, lexer)) {
            case SCN_STOP:
                mrk_end(scanner, lexer);
            case SCN_FAIL:
                RET_SYM(result_symbol);
            default:
                break;
        }
    }
}

static bool scn_dir_rsv_prm(Scanner *scanner, TSLexer *lexer, TSSymbol result_symbol) {
    if (!is_ns_char(lexer->lookahead)) {
        return false;
    }
    adv(scanner, lexer);
    while (is_ns_char(lexer->lookahead)) {
        adv(scanner, lexer);
    }
    mrk_end(scanner, lexer);
    RET_SYM(result_symbol);
}

static bool scn_tag(Scanner *scanner, TSLexer *lexer, TSSymbol result_symbol) {
    if (lexer->lookahead != '!') {
        return false;
    }
    adv(scanner, lexer);
    if (is_wht(lexer->lookahead)) {
        mrk_end(scanner, lexer);
        RET_SYM(result_symbol);
    }
    if (lexer->lookahead == '<') {
        adv(scanner, lexer);
        if (scn_ns_uri_char(scanner, lexer) != SCN_SUCC) {
            return false;
        }
        for (;;) {
            switch (scn_ns_uri_char(scanner, lexer)) {
                case SCN_STOP:
                    if (lexer->lookahead == '>') {
                        adv(scanner, lexer);
                        mrk_end(scanner, lexer);
                        RET_SYM(result_symbol);
                    }
                case SCN_FAIL:
                    return false;
                default:
                    break;
            }
        }
    } else {
        if (scn_tag_hdl_tal(scanner, lexer) && scn_ns_tag_char(scanner, lexer) != SCN_SUCC) {
            return false;
        }
        for (;;) {
            switch (scn_ns_tag_char(scanner, lexer)) {
                case SCN_STOP:
                    mrk_end(scanner, lexer);
                case SCN_FAIL:
                    RET_SYM(result_symbol);
                default:
                    break;
            }
        }
    }
    return false;
}

static bool scn_acr_bgn(Scanner *scanner, TSLexer *lexer, TSSymbol result_symbol) {
    if (lexer->lookahead != '&') {
        return false;
    }
    adv(scanner, lexer);
    if (!is_ns_anchor_char(lexer->lookahead)) {
        return false;
    }
    mrk_end(scanner, lexer);
    RET_SYM(result_symbol);
}

static bool scn_acr_ctn(Scanner *scanner, TSLexer *lexer, TSSymbol result_symbol) {
    while (is_ns_anchor_char(lexer->lookahead)) {
        adv(scanner, lexer);
    }
    mrk_end(scanner, lexer);
    RET_SYM(result_symbol);
}

static bool scn_als_bgn(Scanner *scanner, TSLexer *lexer, TSSymbol result_symbol) {
    if (lexer->lookahead != '*') {
        return false;
    }
    adv(scanner, lexer);
    if (!is_ns_anchor_char(lexer->lookahead)) {
        return false;
    }
    mrk_end(scanner, lexer);
    RET_SYM(result_symbol);
}

static bool scn_als_ctn(Scanner *scanner, TSLexer *lexer, TSSymbol result_symbol) {
    while (is_ns_anchor_char(lexer->lookahead)) {
        adv(scanner, lexer);
    }
    mrk_end(scanner, lexer);
    RET_SYM(result_symbol);
}

static bool scn_dqt_esc_seq(Scanner *scanner, TSLexer *lexer, TSSymbol result_symbol) {
    uint16_t i;
    switch (lexer->lookahead) {
        case '0':
        case 'a':
        case 'b':
        case 't':
        case '\t':
        case 'n':
        case 'v':
        case 'r':
        case 'e':
        case 'f':
        case ' ':
        case '"':
        case '/':
        case '\\':
        case 'N':
        case '_':
        case 'L':
        case 'P':
            adv(scanner, lexer);
            break;
        case 'U':
            adv(scanner, lexer);
            for (i = 0; i < 8; i++) {
                if (is_ns_hex_digit(lexer->lookahead)) {
                    adv(scanner, lexer);
                } else {
                    return false;
                }
            }
            break;
        case 'u':
            adv(scanner, lexer);
            for (i = 0; i < 4; i++) {
                if (is_ns_hex_digit(lexer->lookahead)) {
                    adv(scanner, lexer);
                } else {
                    return false;
                }
            }
            break;
        case 'x':
            adv(scanner, lexer);
            for (i = 0; i < 2; i++) {
                if (is_ns_hex_digit(lexer->lookahead)) {
                    adv(scanner, lexer);
                } else {
                    return false;
                }
            }
            break;
        default:
            return false;
    }
    mrk_end(scanner, lexer);
    RET_SYM(result_symbol);
}

static bool scn_drs_doc_end(Scanner *scanner, TSLexer *lexer) {
    if (lexer->lookahead != '-' && lexer->lookahead != '.') {
        return false;
    }
    int32_t delimeter = lexer->lookahead;
    adv(scanner, lexer);
    if (lexer->lookahead == delimeter) {
        adv(scanner, lexer);
        if (lexer->lookahead == delimeter) {
            adv(scanner, lexer);
            if (is_wht(lexer->lookahead)) {
                return true;
            }
        }
    }
    mrk_end(scanner, lexer);
    return false;
}

static bool scn_dqt_str_cnt(Scanner *scanner, TSLexer *lexer, TSSymbol result_symbol) {
    if (!is_nb_double_char(lexer->lookahead)) {
        return false;
    }
    if (scanner->cur_col == 0 && scn_drs_doc_end(scanner, lexer)) {
        mrk_end(scanner, lexer);
        RET_SYM(scanner->cur_chr == '-' ? S_DRS_END : S_DOC_END);
    } else {
        adv(scanner, lexer);
    }
    while (is_nb_double_char(lexer->lookahead)) {
        adv(scanner, lexer);
    }
    mrk_end(scanner, lexer);
    RET_SYM(result_symbol);
}

static bool scn_sqt_str_cnt(Scanner *scanner, TSLexer *lexer, TSSymbol result_symbol) {
    if (!is_nb_single_char(lexer->lookahead)) {
        return false;
    }
    if (scanner->cur_col == 0 && scn_drs_doc_end(scanner, lexer)) {
        mrk_end(scanner, lexer);
        RET_SYM(scanner->cur_chr == '-' ? S_DRS_END : S_DOC_END);
    } else {
        adv(scanner, lexer);
    }
    while (is_nb_single_char(lexer->lookahead)) {
        adv(scanner, lexer);
    }
    mrk_end(scanner, lexer);
    RET_SYM(result_symbol);
}

static bool scn_blk_str_bgn(Scanner *scanner, TSLexer *lexer, TSSymbol result_symbol) {
    if (lexer->lookahead != '|' && lexer->lookahead != '>') {
        return false;
    }
    adv(scanner, lexer);
    int32_t cur_ind = scanner->indent_lengths[scanner->indent_count - 1];
    int32_t ind = -1;
    if (lexer->lookahead >= '1' && lexer->lookahead <= '9') {
        ind = lexer->lookahead - '1';
        adv(scanner, lexer);
        if (lexer->lookahead == '+' || lexer->lookahead == '-') {
            adv(scanner, lexer);
        }
    } else if (lexer->lookahead == '+' || lexer->lookahead == '-') {
        adv(scanner, lexer);
        if (lexer->lookahead >= '1' && lexer->lookahead <= '9') {
            ind = lexer->lookahead - '1';
            adv(scanner, lexer);
        }
    }
    if (!is_wht(lexer->lookahead)) {
        return false;
    }
    mrk_end(scanner, lexer);
    if (ind != -1) {
        if (cur_ind > INT32_MAX - ind) return false;
        ind += cur_ind;
    } else {
        ind = cur_ind;
        while (is_wsp(lexer->lookahead)) {
            adv(scanner, lexer);
        }
        if (lexer->lookahead == '#') {
            adv(scanner, lexer);
            while (!is_nwl(lexer->lookahead) && lexer->lookahead != 0) {
                adv(scanner, lexer);
            }
        }
        if (is_nwl(lexer->lookahead)) {
            adv_nwl(scanner, lexer);
        }
        while (lexer->lookahead != 0) {
            if (lexer->lookahead == ' ') {
                adv(scanner, lexer);
            } else if (is_nwl(lexer->lookahead)) {
                if (scanner->cur_col - 1 < ind) {
                    break;
                }
                ind = scanner->cur_col - 1;
                adv_nwl(scanner, lexer);
            } else {
                if (scanner->cur_col - 1 > ind) {
                    ind = scanner->cur_col - 1;
                }
                break;
            }
        }
    }
    PUSH_IND(IND_STR, ind);
    RET_SYM(result_symbol);
}

static bool scn_blk_str_cnt(Scanner *scanner, TSLexer *lexer, TSSymbol result_symbol) {
    if (!is_ns_char(lexer->lookahead)) {
        return false;
    }
    if (scanner->cur_col == 0 && scn_drs_doc_end(scanner, lexer)) {
        POP_IND();
        RET_SYM(BL);
    } else {
        adv(scanner, lexer);
    }
    mrk_end(scanner, lexer);
    for (;;) {
        if (is_ns_char(lexer->lookahead)) {
            adv(scanner, lexer);
            while (is_ns_char(lexer->lookahead)) {
                adv(scanner, lexer);
            }
            mrk_end(scanner, lexer);
        }
        if (is_wsp(lexer->lookahead)) {
            adv(scanner, lexer);
            while (is_wsp(lexer->lookahead)) {
                adv(scanner, lexer);
            }
        } else {
            break;
        }
    }
    RET_SYM(result_symbol);
}

static char scn_pln_cnt(Scanner *scanner, TSLexer *lexer, bool (*is_plain_safe)(int32_t)) {
    bool is_cur_wsp = is_wsp(scanner->cur_chr);
    bool is_cur_saf = is_plain_safe(scanner->cur_chr);
    bool is_lka_wsp = is_wsp(lexer->lookahead);
    bool is_lka_saf = is_plain_safe(lexer->lookahead);
    if (is_lka_saf || is_lka_wsp) {
        for (;;) {
            if (is_lka_saf && lexer->lookahead != '#' && lexer->lookahead != ':') {
                adv(scanner, lexer);
                mrk_end(scanner, lexer);
                scanner->sch_stt = adv_sch_stt(scanner->sch_stt, scanner->cur_chr, &scanner->rlt_sch);
            } else if (is_cur_saf && lexer->lookahead == '#') {
                adv(scanner, lexer);
                mrk_end(scanner, lexer);
                scanner->sch_stt = adv_sch_stt(scanner->sch_stt, scanner->cur_chr, &scanner->rlt_sch);
            } else if (is_lka_wsp) {
                adv(scanner, lexer);
                scanner->sch_stt = adv_sch_stt(scanner->sch_stt, scanner->cur_chr, &scanner->rlt_sch);
            } else if (lexer->lookahead == ':') {
                adv(scanner, lexer); // check later
            } else {
                break;
            }

            is_cur_wsp = is_lka_wsp;
            is_cur_saf = is_lka_saf;
            is_lka_wsp = is_wsp(lexer->lookahead);
            is_lka_saf = is_plain_safe(lexer->lookahead);

            if (scanner->cur_chr == ':') {
                if (is_lka_saf) {
                    mrk_end(scanner, lexer);
                    scanner->sch_stt = adv_sch_stt(scanner->sch_stt, scanner->cur_chr, &scanner->rlt_sch);
                } else {
                    return SCN_FAIL;
                }
            }
        }
    } else {
        return SCN_STOP;
    }
    return SCN_SUCC;
}

static bool scan(Scanner *scanner, TSLexer *lexer, const bool *valid_symbols) {
    if (scanner->failed) return false;
    init(scanner);
    mrk_end(scanner, lexer);

    bool allow_comment = !(valid_symbols[R_DQT_STR_CTN] || valid_symbols[BR_DQT_STR_CTN] ||
                           valid_symbols[R_SQT_STR_CTN] || valid_symbols[BR_SQT_STR_CTN]);
    int32_t cur_ind = scanner->indent_lengths[scanner->indent_count - 1];
    int32_t prt_ind = scanner->indent_count > 1 ? scanner->indent_lengths[scanner->indent_count - 2] : -1;
    uint8_t cur_ind_typ = scanner->indent_types[scanner->indent_count - 1];

    bool has_tab_ind = false;
    int32_t leading_spaces = 0;

    for (;;) {
        if (lexer->lookahead == ' ') {
            if (!has_tab_ind) {
                increment(scanner, &leading_spaces);
            }
            skp(scanner, lexer);
        } else if (lexer->lookahead == '\t') {
            has_tab_ind = true;
            skp(scanner, lexer);
        } else if (is_nwl(lexer->lookahead)) {
            has_tab_ind = false;
            leading_spaces = 0;
            skp_nwl(scanner, lexer);
        } else if (allow_comment && lexer->lookahead == '#') {
            if (valid_symbols[BR_BLK_STR_CTN] && valid_symbols[BL] && scanner->cur_col <= cur_ind) {
                POP_IND();
                RET_SYM(BL);
            }
            if (valid_symbols[BR_BLK_STR_CTN]
                    ? scanner->cur_row == scanner->row
                    : scanner->cur_col == 0 || scanner->cur_row != scanner->row || scanner->cur_col > scanner->col) {
                adv(scanner, lexer);
                while (!is_nwl(lexer->lookahead) && lexer->lookahead != 0) {
                    adv(scanner, lexer);
                }
                mrk_end(scanner, lexer);
                RET_SYM(COMMENT);
            } else {
                break;
            }
        } else {
            break;
        }
    }

    if (lexer->lookahead == 0) {
        if (valid_symbols[BL]) {
            mrk_end(scanner, lexer);
            POP_IND();
            RET_SYM(BL)
        }
        if (valid_symbols[END_OF_FILE]) {
            mrk_end(scanner, lexer);
            RET_SYM(END_OF_FILE)
        }
        return false;
    }

    int32_t bgn_row = scanner->cur_row;
    int32_t bgn_col = scanner->cur_col;
    int32_t bgn_chr = lexer->lookahead;

    if (valid_symbols[BL] && bgn_col <= cur_ind && !has_tab_ind) {
        if (cur_ind == prt_ind && cur_ind_typ == IND_SEQ ? bgn_col < cur_ind || lexer->lookahead != '-'
                                                         : bgn_col <= prt_ind || cur_ind_typ == IND_STR) {
            POP_IND();
            RET_SYM(BL);
        }
    }

    bool has_nwl = scanner->cur_row > scanner->row;
    bool is_r = !has_nwl;
    bool is_br = has_nwl && leading_spaces > cur_ind;
    bool is_b = has_nwl && leading_spaces == cur_ind && !has_tab_ind;
    bool is_s = bgn_col == 0;

    if (valid_symbols[R_DIR_YML_VER] && is_r) {
        return scn_dir_yml_ver(scanner, lexer, R_DIR_YML_VER);
    }
    if (valid_symbols[R_DIR_TAG_HDL] && is_r) {
        return scn_dir_tag_hdl(scanner, lexer, R_DIR_TAG_HDL);
    }
    if (valid_symbols[R_DIR_TAG_PFX] && is_r) {
        return scn_dir_tag_pfx(scanner, lexer, R_DIR_TAG_PFX);
    }
    if (valid_symbols[R_DIR_RSV_PRM] && is_r) {
        return scn_dir_rsv_prm(scanner, lexer, R_DIR_RSV_PRM);
    }
    if (valid_symbols[BR_BLK_STR_CTN] && is_br && scn_blk_str_cnt(scanner, lexer, BR_BLK_STR_CTN)) {
        return true;
    }

    if ((valid_symbols[R_DQT_STR_CTN] && is_r && scn_dqt_str_cnt(scanner, lexer, R_DQT_STR_CTN)) ||
        (valid_symbols[BR_DQT_STR_CTN] && is_br && scn_dqt_str_cnt(scanner, lexer, BR_DQT_STR_CTN))) {
        return true;
    }

    if ((valid_symbols[R_SQT_STR_CTN] && is_r && scn_sqt_str_cnt(scanner, lexer, R_SQT_STR_CTN)) ||
        (valid_symbols[BR_SQT_STR_CTN] && is_br && scn_sqt_str_cnt(scanner, lexer, BR_SQT_STR_CTN))) {
        return true;
    }

    if (valid_symbols[R_ACR_CTN] && is_r) {
        return scn_acr_ctn(scanner, lexer, R_ACR_CTN);
    }
    if (valid_symbols[R_ALS_CTN] && is_r) {
        return scn_als_ctn(scanner, lexer, R_ALS_CTN);
    }

    if (lexer->lookahead == '%') {
        if (valid_symbols[S_DIR_YML_BGN] && is_s) {
            return scn_dir_bgn(scanner, lexer);
        }
    } else if (lexer->lookahead == '*') {
        if (valid_symbols[R_ALS_BGN] && is_r) {
            MAY_UPD_IMP_COL();
            return scn_als_bgn(scanner, lexer, R_ALS_BGN);
        }
        if (valid_symbols[BR_ALS_BGN] && is_br) {
            MAY_UPD_IMP_COL();
            return scn_als_bgn(scanner, lexer, BR_ALS_BGN);
        }
        if (valid_symbols[B_ALS_BGN] && is_b) {
            MAY_UPD_IMP_COL();
            return scn_als_bgn(scanner, lexer, B_ALS_BGN);
        }
    } else if (lexer->lookahead == '&') {
        if (valid_symbols[R_ACR_BGN] && is_r) {
            MAY_UPD_IMP_COL();
            return scn_acr_bgn(scanner, lexer, R_ACR_BGN);
        }
        if (valid_symbols[BR_ACR_BGN] && is_br) {
            MAY_UPD_IMP_COL();
            return scn_acr_bgn(scanner, lexer, BR_ACR_BGN);
        }
        if (valid_symbols[B_ACR_BGN] && is_b) {
            MAY_UPD_IMP_COL();
            return scn_acr_bgn(scanner, lexer, B_ACR_BGN);
        }
    } else if (lexer->lookahead == '!') {
        if (valid_symbols[R_TAG] && is_r) {
            MAY_UPD_IMP_COL();
            return scn_tag(scanner, lexer, R_TAG);
        }
        if (valid_symbols[BR_TAG] && is_br) {
            MAY_UPD_IMP_COL();
            return scn_tag(scanner, lexer, BR_TAG);
        }
        if (valid_symbols[B_TAG] && is_b) {
            MAY_UPD_IMP_COL();
            return scn_tag(scanner, lexer, B_TAG);
        }
    } else if (lexer->lookahead == '[') {
        if (valid_symbols[R_FLW_SEQ_BGN] && is_r) {
            MAY_UPD_IMP_COL();
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(R_FLW_SEQ_BGN)
        }
        if (valid_symbols[BR_FLW_SEQ_BGN] && is_br) {
            MAY_UPD_IMP_COL();
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(BR_FLW_SEQ_BGN)
        }
        if (valid_symbols[B_FLW_SEQ_BGN] && is_b) {
            MAY_UPD_IMP_COL();
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(B_FLW_SEQ_BGN)
        }
    } else if (lexer->lookahead == ']') {
        if (valid_symbols[R_FLW_SEQ_END] && is_r) {
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(R_FLW_SEQ_END)
        }
        if (valid_symbols[BR_FLW_SEQ_END] && is_br) {
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(BR_FLW_SEQ_END)
        }
        if (valid_symbols[B_FLW_SEQ_END] && is_b) {
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(BR_FLW_SEQ_END)
        }
    } else if (lexer->lookahead == '{') {
        if (valid_symbols[R_FLW_MAP_BGN] && is_r) {
            MAY_UPD_IMP_COL();
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(R_FLW_MAP_BGN)
        }
        if (valid_symbols[BR_FLW_MAP_BGN] && is_br) {
            MAY_UPD_IMP_COL();
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(BR_FLW_MAP_BGN)
        }
        if (valid_symbols[B_FLW_MAP_BGN] && is_b) {
            MAY_UPD_IMP_COL();
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(B_FLW_MAP_BGN)
        }
    } else if (lexer->lookahead == '}') {
        if (valid_symbols[R_FLW_MAP_END] && is_r) {
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(R_FLW_MAP_END)
        }
        if (valid_symbols[BR_FLW_MAP_END] && is_br) {
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(BR_FLW_MAP_END)
        }
        if (valid_symbols[B_FLW_MAP_END] && is_b) {
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(BR_FLW_MAP_END)
        }
    } else if (lexer->lookahead == ',') {
        if (valid_symbols[R_FLW_SEP_BGN] && is_r) {
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(R_FLW_SEP_BGN)
        }
        if (valid_symbols[BR_FLW_SEP_BGN] && is_br) {
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(BR_FLW_SEP_BGN)
        }
    } else if (lexer->lookahead == '"') {
        if (valid_symbols[R_DQT_STR_BGN] && is_r) {
            MAY_UPD_IMP_COL();
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(R_DQT_STR_BGN)
        }
        if (valid_symbols[BR_DQT_STR_BGN] && is_br) {
            MAY_UPD_IMP_COL();
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(BR_DQT_STR_BGN)
        }
        if (valid_symbols[B_DQT_STR_BGN] && is_b) {
            MAY_UPD_IMP_COL();
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(B_DQT_STR_BGN)
        }
        if (valid_symbols[R_DQT_STR_END] && is_r) {
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(R_DQT_STR_END)
        }
        if (valid_symbols[BR_DQT_STR_END] && is_br) {
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(BR_DQT_STR_END)
        }
    } else if (lexer->lookahead == '\'') {
        if (valid_symbols[R_SQT_STR_BGN] && is_r) {
            MAY_UPD_IMP_COL();
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(R_SQT_STR_BGN)
        }
        if (valid_symbols[BR_SQT_STR_BGN] && is_br) {
            MAY_UPD_IMP_COL();
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(BR_SQT_STR_BGN)
        }
        if (valid_symbols[B_SQT_STR_BGN] && is_b) {
            MAY_UPD_IMP_COL();
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(B_SQT_STR_BGN)
        }
        if (valid_symbols[R_SQT_STR_END] && is_r) {
            adv(scanner, lexer);
            if (lexer->lookahead == '\'') {
                adv(scanner, lexer);
                mrk_end(scanner, lexer);
                RET_SYM(R_SQT_ESC_SQT)
            } else {
                mrk_end(scanner, lexer);
                RET_SYM(R_SQT_STR_END)
            }
        }
        if (valid_symbols[BR_SQT_STR_END] && is_br) {
            adv(scanner, lexer);
            if (lexer->lookahead == '\'') {
                adv(scanner, lexer);
                mrk_end(scanner, lexer);
                RET_SYM(BR_SQT_ESC_SQT)
            } else {
                mrk_end(scanner, lexer);
                RET_SYM(BR_SQT_STR_END)
            }
        }
    } else if (lexer->lookahead == '?') {
        bool is_r_blk_key_bgn = valid_symbols[R_BLK_KEY_BGN] && is_r;
        bool is_br_blk_key_bgn = valid_symbols[BR_BLK_KEY_BGN] && is_br;
        bool is_b_blk_key_bgn = valid_symbols[B_BLK_KEY_BGN] && is_b;
        bool is_r_flw_key_bgn = valid_symbols[R_FLW_KEY_BGN] && is_r;
        bool is_br_flw_key_bgn = valid_symbols[BR_FLW_KEY_BGN] && is_br;
        if (is_r_blk_key_bgn || is_br_blk_key_bgn || is_b_blk_key_bgn || is_r_flw_key_bgn || is_br_flw_key_bgn) {
            adv(scanner, lexer);
            if (is_wht(lexer->lookahead)) {
                mrk_end(scanner, lexer);
                if (is_r_blk_key_bgn) {
                    PUSH_BGN_IND(IND_MAP);
                    RET_SYM(R_BLK_KEY_BGN);
                }
                if (is_br_blk_key_bgn) {
                    PUSH_BGN_IND(IND_MAP);
                    RET_SYM(BR_BLK_KEY_BGN);
                }
                if (is_b_blk_key_bgn)
                    RET_SYM(B_BLK_KEY_BGN);
                if (is_r_flw_key_bgn)
                    RET_SYM(R_FLW_KEY_BGN);
                if (is_br_flw_key_bgn)
                    RET_SYM(BR_FLW_KEY_BGN);
            }
        }
    } else if (lexer->lookahead == ':') {
        if (valid_symbols[R_FLW_JSV_BGN] && is_r) {
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(R_FLW_JSV_BGN);
        }
        if (valid_symbols[BR_FLW_JSV_BGN] && is_br) {
            adv(scanner, lexer);
            mrk_end(scanner, lexer);
            RET_SYM(BR_FLW_JSV_BGN);
        }
        bool is_r_blk_val_bgn = valid_symbols[R_BLK_VAL_BGN] && is_r;
        bool is_br_blk_val_bgn = valid_symbols[BR_BLK_VAL_BGN] && is_br;
        bool is_b_blk_val_bgn = valid_symbols[B_BLK_VAL_BGN] && is_b;
        bool is_r_blk_imp_bgn = valid_symbols[R_BLK_IMP_BGN] && is_r;
        bool is_r_flw_njv_bgn = valid_symbols[R_FLW_NJV_BGN] && is_r;
        bool is_br_flw_njv_bgn = valid_symbols[BR_FLW_NJV_BGN] && is_br;
        if (is_r_blk_val_bgn || is_br_blk_val_bgn || is_b_blk_val_bgn || is_r_blk_imp_bgn || is_r_flw_njv_bgn ||
            is_br_flw_njv_bgn) {
            adv(scanner, lexer);
            bool is_lka_wht = is_wht(lexer->lookahead);
            if (is_lka_wht) {
                if (is_r_blk_val_bgn) {
                    PUSH_BGN_IND(IND_MAP);
                    mrk_end(scanner, lexer);
                    RET_SYM(R_BLK_VAL_BGN);
                }
                if (is_br_blk_val_bgn) {
                    PUSH_BGN_IND(IND_MAP);
                    mrk_end(scanner, lexer);
                    RET_SYM(BR_BLK_VAL_BGN);
                }
                if (is_b_blk_val_bgn) {
                    mrk_end(scanner, lexer);
                    RET_SYM(B_BLK_VAL_BGN);
                }
                if (is_r_blk_imp_bgn) {
                    MAY_PUSH_IMP_IND();
                    mrk_end(scanner, lexer);
                    RET_SYM(R_BLK_IMP_BGN);
                }
            }
            if (is_lka_wht || lexer->lookahead == ',' || lexer->lookahead == ']' || lexer->lookahead == '}') {
                if (is_r_flw_njv_bgn) {
                    mrk_end(scanner, lexer);
                    RET_SYM(R_FLW_NJV_BGN);
                }
                if (is_br_flw_njv_bgn) {
                    mrk_end(scanner, lexer);
                    RET_SYM(BR_FLW_NJV_BGN);
                }
            }
        }
    } else if (lexer->lookahead == '-') {
        bool is_r_blk_seq_bgn = valid_symbols[R_BLK_SEQ_BGN] && is_r;
        bool is_br_blk_seq_bgn = valid_symbols[BR_BLK_SEQ_BGN] && is_br;
        bool is_b_blk_seq_bgn = valid_symbols[B_BLK_SEQ_BGN] && is_b;
        bool is_s_drs_end = is_s;
        if (is_r_blk_seq_bgn || is_br_blk_seq_bgn || is_b_blk_seq_bgn || is_s_drs_end) {
            adv(scanner, lexer);
            if (is_wht(lexer->lookahead)) {
                if (is_r_blk_seq_bgn) {
                    PUSH_BGN_IND(IND_SEQ);
                    mrk_end(scanner, lexer);
                    RET_SYM(R_BLK_SEQ_BGN)
                }
                if (is_br_blk_seq_bgn) {
                    PUSH_BGN_IND(IND_SEQ);
                    mrk_end(scanner, lexer);
                    RET_SYM(BR_BLK_SEQ_BGN)
                }
                if (is_b_blk_seq_bgn) {
                    MAY_PUSH_SPC_SEQ_IND();
                    mrk_end(scanner, lexer);
                    RET_SYM(B_BLK_SEQ_BGN)
                }
            } else if (lexer->lookahead == '-' && is_s_drs_end) {
                adv(scanner, lexer);
                if (lexer->lookahead == '-') {
                    adv(scanner, lexer);
                    if (is_wht(lexer->lookahead)) {
                        if (valid_symbols[BL]) {
                            POP_IND();
                            RET_SYM(BL);
                        }
                        mrk_end(scanner, lexer);
                        RET_SYM(S_DRS_END);
                    }
                }
            }
        }
    } else if (lexer->lookahead == '.') {
        if (is_s) {
            adv(scanner, lexer);
            if (lexer->lookahead == '.') {
                adv(scanner, lexer);
                if (lexer->lookahead == '.') {
                    adv(scanner, lexer);
                    if (is_wht(lexer->lookahead)) {
                        if (valid_symbols[BL]) {
                            POP_IND();
                            RET_SYM(BL);
                        }
                        mrk_end(scanner, lexer);
                        RET_SYM(S_DOC_END);
                    }
                }
            }
        }
    } else if (lexer->lookahead == '\\') {
        bool is_r_dqt_esc_nwl = valid_symbols[R_DQT_ESC_NWL] && is_r;
        bool is_br_dqt_esc_nwl = valid_symbols[BR_DQT_ESC_NWL] && is_br;
        bool is_r_dqt_esc_seq = valid_symbols[R_DQT_ESC_SEQ] && is_r;
        bool is_br_dqt_esc_seq = valid_symbols[BR_DQT_ESC_SEQ] && is_br;
        if (is_r_dqt_esc_nwl || is_br_dqt_esc_nwl || is_r_dqt_esc_seq || is_br_dqt_esc_seq) {
            adv(scanner, lexer);
            if (is_nwl(lexer->lookahead)) {
                if (is_r_dqt_esc_nwl) {
                    mrk_end(scanner, lexer);
                    RET_SYM(R_DQT_ESC_NWL)
                }
                if (is_br_dqt_esc_nwl) {
                    mrk_end(scanner, lexer);
                    RET_SYM(BR_DQT_ESC_NWL)
                }
            }
            if (is_r_dqt_esc_seq) {
                return scn_dqt_esc_seq(scanner, lexer, R_DQT_ESC_SEQ);
            }
            if (is_br_dqt_esc_seq) {
                return scn_dqt_esc_seq(scanner, lexer, BR_DQT_ESC_SEQ);
            }
            return false;
        }
    } else if (lexer->lookahead == '|') {
        if (valid_symbols[R_BLK_LIT_BGN] && is_r) {
            return scn_blk_str_bgn(scanner, lexer, R_BLK_LIT_BGN);
        }
        if (valid_symbols[BR_BLK_LIT_BGN] && is_br) {
            return scn_blk_str_bgn(scanner, lexer, BR_BLK_LIT_BGN);
        }
    } else if (lexer->lookahead == '>') {
        if (valid_symbols[R_BLK_FLD_BGN] && is_r) {
            return scn_blk_str_bgn(scanner, lexer, R_BLK_FLD_BGN);
        }
        if (valid_symbols[BR_BLK_FLD_BGN] && is_br) {
            return scn_blk_str_bgn(scanner, lexer, BR_BLK_FLD_BGN);
        }
    }

    bool maybe_sgl_pln_blk = (valid_symbols[R_SGL_PLN_STR_BLK] && is_r) ||
                             (valid_symbols[BR_SGL_PLN_STR_BLK] && is_br) || (valid_symbols[B_SGL_PLN_STR_BLK] && is_b);
    bool maybe_sgl_pln_flw = (valid_symbols[R_SGL_PLN_STR_FLW] && is_r) || (valid_symbols[BR_SGL_PLN_STR_FLW] && is_br);
    bool maybe_mtl_pln_blk = (valid_symbols[R_MTL_PLN_STR_BLK] && is_r) || (valid_symbols[BR_MTL_PLN_STR_BLK] && is_br);
    bool maybe_mtl_pln_flw = (valid_symbols[R_MTL_PLN_STR_FLW] && is_r) || (valid_symbols[BR_MTL_PLN_STR_FLW] && is_br);

    if (maybe_sgl_pln_blk || maybe_sgl_pln_flw || maybe_mtl_pln_blk || maybe_mtl_pln_flw) {
        bool is_in_blk = maybe_sgl_pln_blk || maybe_mtl_pln_blk;
        bool (*is_plain_safe)(int32_t) = is_in_blk ? is_plain_safe_in_block : is_plain_safe_in_flow;
        if (scanner->cur_col - bgn_col == 0) {
            adv(scanner, lexer);
        }
        if (scanner->cur_col - bgn_col == 1) {
            bool is_plain_first =
                (is_ns_char(bgn_chr) && !is_c_indicator(bgn_chr)) ||
                ((bgn_chr == '-' || bgn_chr == '?' || bgn_chr == ':') && is_plain_safe(lexer->lookahead));
            if (!is_plain_first) {
                return false;
            }
            scanner->sch_stt = adv_sch_stt(scanner->sch_stt, scanner->cur_chr, &scanner->rlt_sch);
        } else {
            // no need to check the following cases:
            // ..X
            // ...X
            // --X
            // ---X
            // X: lookahead
            scanner->sch_stt = SCH_STT_FRZ; // must be RS_STR
        }

        mrk_end(scanner, lexer);

        for (;;) {
            if (!is_nwl(lexer->lookahead)) {
                if (scn_pln_cnt(scanner, lexer, is_plain_safe) != SCN_SUCC) {
                    break;
                }
            }
            if (lexer->lookahead == 0 || !is_nwl(lexer->lookahead)) {
                break;
            }
            for (;;) {
                if (is_nwl(lexer->lookahead)) {
                    adv_nwl(scanner, lexer);
                } else if (is_wsp(lexer->lookahead)) {
                    adv(scanner, lexer);
                } else {
                    break;
                }
            }
            if (lexer->lookahead == 0 || scanner->cur_col <= cur_ind) {
                break;
            }
            if (scanner->cur_col == 0 && scn_drs_doc_end(scanner, lexer)) {
                break;
            }
        }

        if (scanner->end_row == bgn_row) {
            if (maybe_sgl_pln_blk) {
                MAY_UPD_IMP_COL();
                RET_SYM(is_r ? SGL_PLN_SYM(R, BLK) : is_br ? SGL_PLN_SYM(BR, BLK) : SGL_PLN_SYM(B, BLK));
            }
            if (maybe_sgl_pln_flw)
                RET_SYM(is_r ? SGL_PLN_SYM(R, FLW) : SGL_PLN_SYM(BR, FLW));
        } else {
            if (maybe_mtl_pln_blk) {
                MAY_UPD_IMP_COL();
                RET_SYM(is_r ? R_MTL_PLN_STR_BLK : BR_MTL_PLN_STR_BLK);
            }
            if (maybe_mtl_pln_flw)
                RET_SYM(is_r ? R_MTL_PLN_STR_FLW : BR_MTL_PLN_STR_FLW);
        }

        return false;
    }

    return !scanner->failed && !valid_symbols[ERR_REC];
}

void *tree_sitter_yaml_external_scanner_create() {
    Scanner *scanner = ts_calloc(1, sizeof(Scanner));
    if (scanner) reset(scanner);
    return scanner;
}

void tree_sitter_yaml_external_scanner_destroy(void *payload) {
    Scanner *scanner = (Scanner *)payload;
    ts_free(scanner);
}

unsigned tree_sitter_yaml_external_scanner_serialize(void *payload, char *buffer) {
    Scanner *scanner = (Scanner *)payload;
    return scanner ? serialize(scanner, buffer) : 0;
}

void tree_sitter_yaml_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
    Scanner *scanner = (Scanner *)payload;
    if (scanner) deserialize(scanner, buffer, length);
}

bool tree_sitter_yaml_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
    Scanner *scanner = (Scanner *)payload;
    return scanner && scan(scanner, lexer, valid_symbols);
}
