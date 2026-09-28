/* What `c_hello` compiles with the toolchain's clang and runs on ToyOS. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(void) {
    const char *who = "ToyOS";
    char *copy = malloc(strlen(who) + 1);
    if (copy == NULL)
        return 1;
    strcpy(copy, who);
    printf("hello from clang, on %s: %d * %d = %d\n", copy, 6, 7, 6 * 7);
    free(copy);
    return 0;
}
