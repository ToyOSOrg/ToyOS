#ifndef _WCHAR_H
#define _WCHAR_H

#include <stddef.h>
#include <stdarg.h>
#include <stdint.h>
#include <bits/types/FILE.h>
#include <bits/types/locale_t.h>
#include <bits/types/mbstate_t.h>
#include <bits/types/wint_t.h>

#ifdef __cplusplus
extern "C" {
#endif

struct tm;

size_t wcslen(const wchar_t *s);
wchar_t *wcscpy(wchar_t *dest, const wchar_t *src);
wchar_t *wcsncpy(wchar_t *dest, const wchar_t *src, size_t n);
wchar_t *wcscat(wchar_t *dest, const wchar_t *src);
wchar_t *wcsncat(wchar_t *dest, const wchar_t *src, size_t n);
int wcscmp(const wchar_t *a, const wchar_t *b);
int wcsncmp(const wchar_t *a, const wchar_t *b, size_t n);
int wcscoll(const wchar_t *a, const wchar_t *b);
int wcscoll_l(const wchar_t *a, const wchar_t *b, locale_t loc);
size_t wcsxfrm(wchar_t *dest, const wchar_t *src, size_t n);
size_t wcsxfrm_l(wchar_t *dest, const wchar_t *src, size_t n, locale_t loc);
wchar_t *wcschr(const wchar_t *s, wchar_t c);
wchar_t *wcsrchr(const wchar_t *s, wchar_t c);
size_t wcsspn(const wchar_t *s, const wchar_t *accept);
size_t wcscspn(const wchar_t *s, const wchar_t *reject);
wchar_t *wcspbrk(const wchar_t *s, const wchar_t *accept);
wchar_t *wcsstr(const wchar_t *haystack, const wchar_t *needle);
wchar_t *wcstok(wchar_t *s, const wchar_t *delim, wchar_t **save);

wchar_t *wmemchr(const wchar_t *s, wchar_t c, size_t n);
int wmemcmp(const wchar_t *a, const wchar_t *b, size_t n);
wchar_t *wmemcpy(wchar_t *dest, const wchar_t *src, size_t n);
wchar_t *wmemmove(wchar_t *dest, const wchar_t *src, size_t n);
wchar_t *wmemset(wchar_t *dest, wchar_t c, size_t n);

long wcstol(const wchar_t *s, wchar_t **endptr, int base);
unsigned long wcstoul(const wchar_t *s, wchar_t **endptr, int base);
long long wcstoll(const wchar_t *s, wchar_t **endptr, int base);
unsigned long long wcstoull(const wchar_t *s, wchar_t **endptr, int base);
float wcstof(const wchar_t *s, wchar_t **endptr);
double wcstod(const wchar_t *s, wchar_t **endptr);
long double wcstold(const wchar_t *s, wchar_t **endptr);

wint_t btowc(int c);
int wctob(wint_t c);
int mbsinit(const mbstate_t *ps);
size_t mbrlen(const char *s, size_t n, mbstate_t *ps);
size_t mbrtowc(wchar_t *pwc, const char *s, size_t n, mbstate_t *ps);
size_t wcrtomb(char *s, wchar_t wc, mbstate_t *ps);
size_t mbsrtowcs(wchar_t *dest, const char **src, size_t len, mbstate_t *ps);
size_t wcsrtombs(char *dest, const wchar_t **src, size_t len, mbstate_t *ps);
size_t mbsnrtowcs(wchar_t *dest, const char **src, size_t nms, size_t len, mbstate_t *ps);
size_t wcsnrtombs(char *dest, const wchar_t **src, size_t nwc, size_t len, mbstate_t *ps);

wint_t fgetwc(FILE *stream);
wint_t getwc(FILE *stream);
wint_t ungetwc(wint_t wc, FILE *stream);
wint_t fputwc(wchar_t wc, FILE *stream);
wint_t putwc(wchar_t wc, FILE *stream);

int swprintf(wchar_t *s, size_t n, const wchar_t *fmt, ...);
int vswprintf(wchar_t *s, size_t n, const wchar_t *fmt, va_list ap);

#ifdef __cplusplus
}
#endif

#endif
