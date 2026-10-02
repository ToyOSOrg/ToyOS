#ifndef _INTTYPES_H
#define _INTTYPES_H

#include <stdint.h>

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    intmax_t quot;
    intmax_t rem;
} imaxdiv_t;

intmax_t imaxabs(intmax_t j);
imaxdiv_t imaxdiv(intmax_t numer, intmax_t denom);
intmax_t strtoimax(const char *nptr, char **endptr, int base);
uintmax_t strtoumax(const char *nptr, char **endptr, int base);
intmax_t wcstoimax(const wchar_t *nptr, wchar_t **endptr, int base);
uintmax_t wcstoumax(const wchar_t *nptr, wchar_t **endptr, int base);

#define PRId8 __INT8_FMTd__
#define PRId16 __INT16_FMTd__
#define PRId32 __INT32_FMTd__
#define PRId64 __INT64_FMTd__
#define PRIdLEAST8 __INT_LEAST8_FMTd__
#define PRIdLEAST16 __INT_LEAST16_FMTd__
#define PRIdLEAST32 __INT_LEAST32_FMTd__
#define PRIdLEAST64 __INT_LEAST64_FMTd__
#define PRIdFAST8 __INT_FAST8_FMTd__
#define PRIdFAST16 __INT_FAST16_FMTd__
#define PRIdFAST32 __INT_FAST32_FMTd__
#define PRIdFAST64 __INT_FAST64_FMTd__
#define PRIdMAX __INTMAX_FMTd__
#define PRIdPTR __INTPTR_FMTd__

#define PRIi8 __INT8_FMTi__
#define PRIi16 __INT16_FMTi__
#define PRIi32 __INT32_FMTi__
#define PRIi64 __INT64_FMTi__
#define PRIiLEAST8 __INT_LEAST8_FMTi__
#define PRIiLEAST16 __INT_LEAST16_FMTi__
#define PRIiLEAST32 __INT_LEAST32_FMTi__
#define PRIiLEAST64 __INT_LEAST64_FMTi__
#define PRIiFAST8 __INT_FAST8_FMTi__
#define PRIiFAST16 __INT_FAST16_FMTi__
#define PRIiFAST32 __INT_FAST32_FMTi__
#define PRIiFAST64 __INT_FAST64_FMTi__
#define PRIiMAX __INTMAX_FMTi__
#define PRIiPTR __INTPTR_FMTi__

#define PRIo8 __UINT8_FMTo__
#define PRIo16 __UINT16_FMTo__
#define PRIo32 __UINT32_FMTo__
#define PRIo64 __UINT64_FMTo__
#define PRIoLEAST8 __UINT_LEAST8_FMTo__
#define PRIoLEAST16 __UINT_LEAST16_FMTo__
#define PRIoLEAST32 __UINT_LEAST32_FMTo__
#define PRIoLEAST64 __UINT_LEAST64_FMTo__
#define PRIoFAST8 __UINT_FAST8_FMTo__
#define PRIoFAST16 __UINT_FAST16_FMTo__
#define PRIoFAST32 __UINT_FAST32_FMTo__
#define PRIoFAST64 __UINT_FAST64_FMTo__
#define PRIoMAX __UINTMAX_FMTo__
#define PRIoPTR __UINTPTR_FMTo__

#define PRIu8 __UINT8_FMTu__
#define PRIu16 __UINT16_FMTu__
#define PRIu32 __UINT32_FMTu__
#define PRIu64 __UINT64_FMTu__
#define PRIuLEAST8 __UINT_LEAST8_FMTu__
#define PRIuLEAST16 __UINT_LEAST16_FMTu__
#define PRIuLEAST32 __UINT_LEAST32_FMTu__
#define PRIuLEAST64 __UINT_LEAST64_FMTu__
#define PRIuFAST8 __UINT_FAST8_FMTu__
#define PRIuFAST16 __UINT_FAST16_FMTu__
#define PRIuFAST32 __UINT_FAST32_FMTu__
#define PRIuFAST64 __UINT_FAST64_FMTu__
#define PRIuMAX __UINTMAX_FMTu__
#define PRIuPTR __UINTPTR_FMTu__

#define PRIx8 __UINT8_FMTx__
#define PRIx16 __UINT16_FMTx__
#define PRIx32 __UINT32_FMTx__
#define PRIx64 __UINT64_FMTx__
#define PRIxLEAST8 __UINT_LEAST8_FMTx__
#define PRIxLEAST16 __UINT_LEAST16_FMTx__
#define PRIxLEAST32 __UINT_LEAST32_FMTx__
#define PRIxLEAST64 __UINT_LEAST64_FMTx__
#define PRIxFAST8 __UINT_FAST8_FMTx__
#define PRIxFAST16 __UINT_FAST16_FMTx__
#define PRIxFAST32 __UINT_FAST32_FMTx__
#define PRIxFAST64 __UINT_FAST64_FMTx__
#define PRIxMAX __UINTMAX_FMTx__
#define PRIxPTR __UINTPTR_FMTx__

#define PRIX8 __UINT8_FMTX__
#define PRIX16 __UINT16_FMTX__
#define PRIX32 __UINT32_FMTX__
#define PRIX64 __UINT64_FMTX__
#define PRIXLEAST8 __UINT_LEAST8_FMTX__
#define PRIXLEAST16 __UINT_LEAST16_FMTX__
#define PRIXLEAST32 __UINT_LEAST32_FMTX__
#define PRIXLEAST64 __UINT_LEAST64_FMTX__
#define PRIXFAST8 __UINT_FAST8_FMTX__
#define PRIXFAST16 __UINT_FAST16_FMTX__
#define PRIXFAST32 __UINT_FAST32_FMTX__
#define PRIXFAST64 __UINT_FAST64_FMTX__
#define PRIXMAX __UINTMAX_FMTX__
#define PRIXPTR __UINTPTR_FMTX__

#define SCNd32 "d"
#define SCNd64 "ld"
#define SCNi32 "i"
#define SCNi64 "li"
#define SCNu32 "u"
#define SCNu64 "lu"
#define SCNx32 "x"
#define SCNx64 "lx"

#ifdef __cplusplus
}
#endif

#endif
