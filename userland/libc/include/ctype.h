#ifndef _CTYPE_H
#define _CTYPE_H

#include <bits/types/locale_t.h>

#ifdef __cplusplus
extern "C" {
#endif

int isalpha(int c);
int isdigit(int c);
int isalnum(int c);
int isblank(int c);
int isspace(int c);
int isupper(int c);
int islower(int c);
int isprint(int c);
int ispunct(int c);
int isxdigit(int c);
int iscntrl(int c);
int isgraph(int c);
int toupper(int c);
int tolower(int c);

int isalpha_l(int c, locale_t loc);
int isdigit_l(int c, locale_t loc);
int isalnum_l(int c, locale_t loc);
int isblank_l(int c, locale_t loc);
int isspace_l(int c, locale_t loc);
int isupper_l(int c, locale_t loc);
int islower_l(int c, locale_t loc);
int isprint_l(int c, locale_t loc);
int ispunct_l(int c, locale_t loc);
int isxdigit_l(int c, locale_t loc);
int iscntrl_l(int c, locale_t loc);
int isgraph_l(int c, locale_t loc);
int toupper_l(int c, locale_t loc);
int tolower_l(int c, locale_t loc);

#ifdef __cplusplus
}
#endif

#endif
