/* Rootlight: complete bounded scanner snapshots, never truncated tag identities.
 * The format is private to this scanner build; malformed snapshots clear state.
 */
#ifndef ROOTLIGHT_HTML_STATE_H
#define ROOTLIGHT_HTML_STATE_H

#define HTML_STATE_HEADER 3u
#define HTML_MAX_NAME_BYTES (TREE_SITTER_SERIALIZATION_BUFFER_SIZE - HTML_STATE_HEADER - 3u)

typedef struct {
    Array(Tag) tags;
    unsigned serialized_bytes;
} Scanner;

static unsigned tag_size(const Tag *tag) {
    return 1u + (tag->type == CUSTOM ? 2u + tag->custom_tag_name.size : 0u);
}

static void clear_tags(Scanner *scanner) {
    for (unsigned i = 0; i < scanner->tags.size; i++) tag_free(&scanner->tags.contents[i]);
    array_clear(&scanner->tags);
    scanner->serialized_bytes = HTML_STATE_HEADER;
}

static unsigned read_u16(const char *buffer) {
    return (uint8_t)buffer[0] | ((unsigned)(uint8_t)buffer[1] << 8);
}

static void write_u16(char *buffer, unsigned value) {
    buffer[0] = (char)(value & 255u);
    buffer[1] = (char)((value >> 8) & 255u);
}

static unsigned serialize(Scanner *scanner, char *buffer) {
    buffer[0] = 1;
    write_u16(buffer + 1, scanner->tags.size);
    unsigned size = HTML_STATE_HEADER;
    for (unsigned i = 0; i < scanner->tags.size; i++) {
        const Tag *tag = &scanner->tags.contents[i];
        buffer[size++] = (char)tag->type;
        if (tag->type == CUSTOM) {
            unsigned length = tag->custom_tag_name.size;
            write_u16(buffer + size, length);
            size += 2;
            memcpy(buffer + size, tag->custom_tag_name.contents, length);
            size += length;
        }
    }
    return size;
}

static void deserialize(Scanner *scanner, const char *buffer, unsigned length) {
    clear_tags(scanner);
    if (!buffer || length < HTML_STATE_HEADER ||
        length > TREE_SITTER_SERIALIZATION_BUFFER_SIZE || buffer[0] != 1) return;
    unsigned count = read_u16(buffer + 1);
    unsigned position = HTML_STATE_HEADER;
    /* Validate the entire frame before allocating or exposing any restored tag. */
    for (unsigned i = 0; i < count; i++) {
        if (position >= length) return;
        unsigned type = (uint8_t)buffer[position++];
        if (type >= END_ || type == END_OF_VOID_TAGS) return;
        if (type == CUSTOM) {
            if (length - position < 2) return;
            unsigned bytes = read_u16(buffer + position);
            position += 2;
            if (bytes == 0 || bytes > length - position) return;
            position += bytes;
        }
    }
    if (position != length) return;
    array_reserve(&scanner->tags, count);
    position = HTML_STATE_HEADER;
    for (unsigned i = 0; i < count; i++) {
        Tag tag = tag_new();
        tag.type = (TagType)(uint8_t)buffer[position++];
        if (tag.type == CUSTOM) {
            unsigned bytes = read_u16(buffer + position);
            position += 2;
            array_reserve(&tag.custom_tag_name, bytes);
            memcpy(tag.custom_tag_name.contents, buffer + position, bytes);
            tag.custom_tag_name.size = bytes;
            position += bytes;
        }
        array_push(&scanner->tags, tag);
    }
    scanner->serialized_bytes = length;
}

#endif
