#ifndef _BITS_TYPES_MBSTATE_T_H
#define _BITS_TYPES_MBSTATE_T_H

typedef struct {
    unsigned int __bits;
    unsigned int __needed;
} mbstate_t;

#endif
