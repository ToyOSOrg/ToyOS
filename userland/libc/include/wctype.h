#ifndef _WCTYPE_H
#define _WCTYPE_H

#include <bits/types/locale_t.h>
#include <bits/types/wint_t.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef unsigned long wctype_t;
typedef unsigned long wctrans_t;

int iswalnum(wint_t wc);
int iswalpha(wint_t wc);
int iswblank(wint_t wc);
int iswcntrl(wint_t wc);
int iswdigit(wint_t wc);
int iswgraph(wint_t wc);
int iswlower(wint_t wc);
int iswprint(wint_t wc);
int iswpunct(wint_t wc);
int iswspace(wint_t wc);
int iswupper(wint_t wc);
int iswxdigit(wint_t wc);
int iswctype(wint_t wc, wctype_t desc);
wctype_t wctype(const char *name);
wint_t towlower(wint_t wc);
wint_t towupper(wint_t wc);
wint_t towctrans(wint_t wc, wctrans_t desc);
wctrans_t wctrans(const char *name);

int iswalnum_l(wint_t wc, locale_t loc);
int iswalpha_l(wint_t wc, locale_t loc);
int iswblank_l(wint_t wc, locale_t loc);
int iswcntrl_l(wint_t wc, locale_t loc);
int iswdigit_l(wint_t wc, locale_t loc);
int iswgraph_l(wint_t wc, locale_t loc);
int iswlower_l(wint_t wc, locale_t loc);
int iswprint_l(wint_t wc, locale_t loc);
int iswpunct_l(wint_t wc, locale_t loc);
int iswspace_l(wint_t wc, locale_t loc);
int iswupper_l(wint_t wc, locale_t loc);
int iswxdigit_l(wint_t wc, locale_t loc);
int iswctype_l(wint_t wc, wctype_t desc, locale_t loc);
wctype_t wctype_l(const char *name, locale_t loc);
wint_t towlower_l(wint_t wc, locale_t loc);
wint_t towupper_l(wint_t wc, locale_t loc);
wint_t towctrans_l(wint_t wc, wctrans_t desc, locale_t loc);
wctrans_t wctrans_l(const char *name, locale_t loc);

#ifdef __cplusplus
}
#endif

#endif
