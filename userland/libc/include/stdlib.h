#ifndef _STDLIB_H
#define _STDLIB_H

#include <stddef.h>

#include <bits/types/locale_t.h>

#ifdef __cplusplus
extern "C" {
#endif

#define EXIT_SUCCESS 0
#define EXIT_FAILURE 1
#define RAND_MAX 2147483647

/* The C locale is the only one, and its encoding is UTF-8. */
#define MB_CUR_MAX ((size_t)4)

typedef struct { int quot; int rem; } div_t;
typedef struct { long quot; long rem; } ldiv_t;
typedef struct { long long quot; long long rem; } lldiv_t;

void *malloc(size_t size);
void *calloc(size_t nmemb, size_t size);
void *realloc(void *ptr, size_t size);
void free(void *ptr);
void *aligned_alloc(size_t alignment, size_t size);
int posix_memalign(void **memptr, size_t alignment, size_t size);

__attribute__((__noreturn__)) void exit(int status);
__attribute__((__noreturn__)) void _exit(int status);
__attribute__((__noreturn__)) void _Exit(int status);
__attribute__((__noreturn__)) void abort(void);
int atexit(void (*func)(void));
int system(const char *command);

double atof(const char *s);
int atoi(const char *s);
long atol(const char *s);
long long atoll(const char *s);
long strtol(const char *s, char **endptr, int base);
unsigned long strtoul(const char *s, char **endptr, int base);
long long strtoll(const char *s, char **endptr, int base);
unsigned long long strtoull(const char *s, char **endptr, int base);
float strtof(const char *s, char **endptr);
double strtod(const char *s, char **endptr);
long double strtold(const char *s, char **endptr);
float strtof_l(const char *s, char **endptr, locale_t loc);
double strtod_l(const char *s, char **endptr, locale_t loc);
long double strtold_l(const char *s, char **endptr, locale_t loc);

char *getenv(const char *name);
int setenv(const char *name, const char *value, int overwrite);
int unsetenv(const char *name);

void qsort(void *base, size_t nmemb, size_t size, int (*compar)(const void *, const void *));
void *bsearch(const void *key, const void *base, size_t nmemb, size_t size, int (*compar)(const void *, const void *));

int abs(int j);
long labs(long j);
long long llabs(long long j);
div_t div(int numer, int denom);
ldiv_t ldiv(long numer, long denom);
lldiv_t lldiv(long long numer, long long denom);

int mblen(const char *s, size_t n);
int mbtowc(wchar_t *pwc, const char *s, size_t n);
int wctomb(char *s, wchar_t wc);
size_t mbstowcs(wchar_t *dest, const char *src, size_t n);
size_t wcstombs(char *dest, const wchar_t *src, size_t n);

int rand(void);
void srand(unsigned int seed);

#ifdef __cplusplus
}
#endif

#endif
