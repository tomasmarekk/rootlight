/* Observe two pinned complete originals with CUDA grammar and unchanged C++ queries.
 * This child-only syntax observation does not establish CUDA semantics or
 * production admission. Physical bytes and every error/capture are retained.
 */
#include <tree_sitter/api.h>
#include "rootlight_cuda_status.h"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern const TSLanguage *tree_sitter_cuda(void);

static char *read_file(const char *path, uint32_t *length) {
    FILE *file = fopen(path, "rb");
    if (!file || fseek(file, 0, SEEK_END)) return NULL;
    long size = ftell(file);
    if (size < 0 || (unsigned long)size > UINT32_MAX || fseek(file, 0, SEEK_SET)) {
        fclose(file);
        return NULL;
    }
    char *bytes = malloc((size_t)size + 1);
    if (!bytes || fread(bytes, 1, (size_t)size, file) != (size_t)size) {
        free(bytes);
        fclose(file);
        return NULL;
    }
    fclose(file);
    bytes[size] = 0;
    *length = (uint32_t)size;
    return bytes;
}

static const char *kind_name(TSNode node) {
    const char *name = ts_node_type(node);
    /* Anonymous quote tokens need JSON escaping; native types stay unchanged. */
    return strcmp(name, "\"") == 0 ? "\\\"" : name;
}

static void node_record(const char *record, TSNode node, uint32_t depth) {
    TSPoint start = ts_node_start_point(node), end = ts_node_end_point(node);
    printf("{\"record\":\"%s\",\"kind\":\"%s\",\"start\":%u,\"end\":%u,"
           "\"start_row\":%u,\"start_column\":%u,\"end_row\":%u,\"end_column\":%u,"
           "\"depth\":%u,\"error\":%s,\"has_error\":%s,\"missing\":%s}\n",
           record, kind_name(node), ts_node_start_byte(node), ts_node_end_byte(node),
           start.row, start.column, end.row, end.column, depth,
           ts_node_is_error(node) ? "true" : "false",
           ts_node_has_error(node) ? "true" : "false",
           ts_node_is_missing(node) ? "true" : "false");
}

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    uint32_t length = 0, query_length = 0;
    char *source = read_file(argv[1], &length);
    char *query_text = read_file(argv[2], &query_length);
    if (!source || !query_text || (length != 6949 && length != 5192)) return 3;
    uint64_t token = rootlight_cuda_scope_begin(2000000);
    if (!token || !rootlight_cuda_scope_set_cancel(token, NULL, NULL)) return 10;
    const TSLanguage *language = tree_sitter_cuda();
    TSParser *parser = ts_parser_new();
    if (!parser || !ts_parser_set_language(parser, language)) return 4;
    TSTree *tree = ts_parser_parse_string(parser, NULL, source, length);
    if (!tree) return 5;
    TSNode root = ts_tree_root_node(tree);
    node_record("root", root, 0);
    if (ts_node_start_byte(root) || ts_node_end_byte(root) != length) return 6;
    TSTreeCursor cursor = ts_tree_cursor_new(root);
    uint32_t nodes = 0, errors = 0, missing = 0, depth = 0;
    for (;;) {
        TSNode node = ts_tree_cursor_current_node(&cursor);
        nodes++;
        errors += ts_node_is_error(node);
        missing += ts_node_is_missing(node);
        node_record("node", node, depth);
        if (ts_tree_cursor_goto_first_child(&cursor)) { depth++; continue; }
        while (!ts_tree_cursor_goto_next_sibling(&cursor)) {
            if (!ts_tree_cursor_goto_parent(&cursor)) goto finished;
            depth--;
        }
    }
finished:
    ts_tree_cursor_delete(&cursor);
    uint32_t error_offset = 0;
    TSQueryError error_type = TSQueryErrorNone;
    TSQuery *query = ts_query_new(language, query_text, query_length, &error_offset, &error_type);
    if (!query) {
        printf("{\"record\":\"query_error\",\"offset\":%u,\"error_type\":%u}\n", error_offset, error_type);
        return 7;
    }
    TSQueryCursor *queries = ts_query_cursor_new();
    if (!queries) return 8;
    ts_query_cursor_exec(queries, query, root);
    TSQueryMatch match;
    uint32_t capture_index = 0, captures = 0;
    while (ts_query_cursor_next_capture(queries, &match, &capture_index)) {
        TSQueryCapture capture = match.captures[capture_index];
        uint32_t name_length = 0;
        const char *name = ts_query_capture_name_for_id(query, capture.index, &name_length);
        TSNode parent = ts_node_parent(capture.node);
        printf("{\"record\":\"capture\",\"capture\":\"%.*s\",\"kind\":\"%s\","
               "\"start\":%u,\"end\":%u,\"parent_kind\":\"%s\",\"parent_start\":%u,"
               "\"parent_end\":%u,\"has_error\":%s,\"missing\":%s}\n",
               (int)name_length, name, kind_name(capture.node),
               ts_node_start_byte(capture.node), ts_node_end_byte(capture.node),
               ts_node_is_null(parent) ? "" : kind_name(parent),
               ts_node_is_null(parent) ? 0 : ts_node_start_byte(parent),
               ts_node_is_null(parent) ? 0 : ts_node_end_byte(parent),
               ts_node_has_error(capture.node) ? "true" : "false",
               ts_node_is_missing(capture.node) ? "true" : "false");
        captures++;
    }
    bool exceeded = ts_query_cursor_did_exceed_match_limit(queries);
    printf("{\"record\":\"summary\",\"bytes\":%u,\"abi\":%u,\"nodes\":%u,\"errors\":%u,"
           "\"missing\":%u,\"captures\":%u,\"query_match_limit_exceeded\":%s}\n",
           length, ts_language_abi_version(language), nodes, errors, missing, captures,
           exceeded ? "true" : "false");
    ts_query_cursor_delete(queries);
    ts_query_delete(query);
    ts_tree_delete(tree);
    ts_parser_delete(parser);
    free(query_text);
    free(source);
    uint64_t work = rootlight_cuda_scope_work(token);
    RootlightCudaStatus status = rootlight_cuda_scope_finish(token);
    printf("{\"record\":\"scanner_scope\",\"status\":%u,\"work\":%llu,\"limit\":2000000}\n", (unsigned)status, (unsigned long long)work);
    return status ? 11 : exceeded ? 9 : 0;
}
