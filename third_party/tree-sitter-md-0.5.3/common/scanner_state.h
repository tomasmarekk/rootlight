// Byte-encoded scanner snapshots are independent of host enum size and alignment.
// Wide source counters preserve delimiters beyond one byte without changing the ABI buffer.
#ifndef ROOTLIGHT_MARKDOWN_SCANNER_STATE_H
#define ROOTLIGHT_MARKDOWN_SCANNER_STATE_H

#include <stdint.h>

static inline void markdown_write_u64(char *buffer, uint64_t value) {
    for (unsigned i = 0; i < 8; i++) {
        buffer[i] = (char)(uint8_t)(value >> (8 * i));
    }
}

static inline uint64_t markdown_read_u64(const char *buffer) {
    uint64_t value = 0;
    for (unsigned i = 0; i < 8; i++) {
        value |= (uint64_t)(uint8_t)buffer[i] << (8 * i);
    }
    return value;
}

static inline void markdown_write_u16(char *buffer, uint16_t value) {
    buffer[0] = (char)(uint8_t)value;
    buffer[1] = (char)(uint8_t)(value >> 8);
}

static inline uint16_t markdown_read_u16(const char *buffer) {
    return (uint16_t)((uint16_t)(uint8_t)buffer[0] |
                      ((uint16_t)(uint8_t)buffer[1] << 8));
}

#endif
