#ifndef _SCHED_H
#define _SCHED_H

#ifdef __cplusplus
extern "C" {
#endif

#include <stddef.h>
#include <sys/types.h>

/* CPU n is bit n % 64 of word n / 64, by the kernel's dense index. */
#define CPU_SETSIZE 1024
typedef struct {
    unsigned long __bits[CPU_SETSIZE / 64];
} cpu_set_t;

#define CPU_ZERO(set) __builtin_memset((set), 0, sizeof(cpu_set_t))
#define CPU_SET(cpu, set) ((void)((cpu) < CPU_SETSIZE && ((set)->__bits[(cpu) / 64] |= 1UL << ((cpu) % 64))))
#define CPU_CLR(cpu, set) ((void)((cpu) < CPU_SETSIZE && ((set)->__bits[(cpu) / 64] &= ~(1UL << ((cpu) % 64)))))
#define CPU_ISSET(cpu, set) ((cpu) < CPU_SETSIZE && ((set)->__bits[(cpu) / 64] >> ((cpu) % 64)) & 1)
#define CPU_COUNT(set) __toyos_cpu_count(set)

static inline int __toyos_cpu_count(const cpu_set_t *set) {
    int n = 0;
    for (int i = 0; i < CPU_SETSIZE / 64; i++) {
        n += __builtin_popcountl(set->__bits[i]);
    }
    return n;
}

int sched_yield(void);
int sched_setaffinity(pid_t pid, size_t cpusetsize, const cpu_set_t *mask);
int sched_getaffinity(pid_t pid, size_t cpusetsize, cpu_set_t *mask);
int sched_getcpu(void);

#ifdef __cplusplus
}
#endif

#endif
