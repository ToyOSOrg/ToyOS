#ifndef _BITS_TYPES_MBSTATE_T_H
#define _BITS_TYPES_MBSTATE_T_H

/* A UTF-8 sequence cut short: the code point's bits so far, and how many
   continuation bytes it still needs. All zero is the initial state. */
typedef struct {
    unsigned int __bits;
    unsigned int __needed;
} mbstate_t;

#endif
