#ifndef _SYS_TIME_H
#define _SYS_TIME_H

#include <time.h>

#ifdef __cplusplus
extern "C" {
#endif

struct timeval {
    long tv_sec;
    long tv_usec;
};

struct timezone {
    int tz_minuteswest;
    int tz_dsttime;
};

int gettimeofday(struct timeval *tv, struct timezone *tz);
int utimes(const char *path, const struct timeval times[2]);

#ifdef __cplusplus
}
#endif

#endif
