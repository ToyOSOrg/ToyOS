/* Constructors run before main in priority order, and exit runs the atexit
 * handlers, then the destructors in reverse priority order, then flushes the
 * streams: the destructor's line is still in stdout's buffer when it does. */
#include <stdio.h>
#include <stdlib.h>

static int constructed;

static void __attribute__((constructor(102))) second(void)
{
    printf("constructor 102 after %d\n", constructed);
    constructed = 102;
}

static void __attribute__((constructor(101))) first(void)
{
    printf("constructor 101\n");
    constructed = 101;
}

static void __attribute__((destructor(101))) last(void)
{
    fputs("destructor 101, unterminated", stdout);
}

static void __attribute__((destructor(102))) before_last(void)
{
    printf("destructor 102\n");
}

static void registered(void)
{
    printf("atexit\n");
}

int main(void)
{
    printf("main after %d\n", constructed);
    atexit(registered);
    exit(0);
}
