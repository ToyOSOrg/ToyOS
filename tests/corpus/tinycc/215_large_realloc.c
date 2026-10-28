/* C's allocator past the size at which a block is a mapping of its own: a
   block grown by realloc from a kibibyte to 64 MiB, doubling, keeps every
   byte as it crosses from the heap into a mapping and grows on, and again
   when it is shrunk back into the heap; calloc's large block reads zeros
   where a freed one of its size was written. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void judge(const char *what, int is) {
    printf("%s: %s\n", what, is ? "yes" : "NO");
}

static unsigned char byte(size_t i) {
    return (unsigned char)(i % 251);
}

static int holds(const unsigned char *p, size_t len) {
    for (size_t i = 0; i < len; i++)
        if (p[i] != byte(i))
            return 0;
    return 1;
}

int main(void) {
    size_t len = 1024;
    unsigned char *p = malloc(len);
    for (size_t i = 0; i < len; i++)
        p[i] = byte(i);
    int kept = 1;
    while (len < (64u << 20)) {
        unsigned char *q = realloc(p, len * 2);
        if (!q) {
            kept = 0;
            break;
        }
        p = q;
        kept &= holds(p, len);
        for (size_t i = len; i < len * 2; i++)
            p[i] = byte(i);
        len *= 2;
    }
    judge("a block grown to 64 MiB keeps its bytes", kept && holds(p, len));

    unsigned char *small = realloc(p, 1024);
    judge("shrunk back to a kibibyte, it keeps its bytes", small && holds(small, 1024));
    free(small);

    size_t big = 8u << 20;
    unsigned char *written = malloc(big);
    memset(written, 0x5a, big);
    free(written);
    unsigned char *zeroed = calloc(big, 1);
    int zeros = zeroed != NULL;
    for (size_t i = 0; zeros && i < big; i++)
        zeros = zeroed[i] == 0;
    judge("a large calloc reads zeros", zeros);
    free(zeroed);
    return 0;
}
