#ifndef _DIRENT_H
#define _DIRENT_H

#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct __dirstream DIR;

struct dirent {
    ino_t d_ino;
    unsigned char d_type;
    char d_name[256];
};

/* What d_type can say. ToyOS's listing tells a directory from everything
   else, so readdir answers DT_DIR or DT_UNKNOWN. */
#define DT_UNKNOWN 0
#define DT_FIFO    1
#define DT_CHR     2
#define DT_DIR     4
#define DT_BLK     6
#define DT_REG     8
#define DT_LNK     10
#define DT_SOCK    12

DIR *opendir(const char *path);
struct dirent *readdir(DIR *dir);
int closedir(DIR *dir);

#ifdef __cplusplus
}
#endif

#endif
