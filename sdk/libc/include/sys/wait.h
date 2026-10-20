#ifndef _SYS_WAIT_H
#define _SYS_WAIT_H

#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

#define WNOHANG   1
#define WUNTRACED 2
#define WCONTINUED 8

#define WEXITSTATUS(s) (((s) >> 8) & 0xff)
#define WTERMSIG(s)    ((s) & 0x7f)
#define WIFEXITED(s)   (WTERMSIG(s) == 0)
#define WIFSIGNALED(s) (WTERMSIG(s) != 0 && WTERMSIG(s) != 0x7f)
/* No child stops or continues, so no status reads as either. */
#define WIFSTOPPED(s)   (((s) & 0xff) == 0x7f)
#define WIFCONTINUED(s) ((s) == 0xffff)

struct rusage;

pid_t waitpid(pid_t pid, int *status, int options);
pid_t wait(int *status);
pid_t wait4(pid_t pid, int *status, int options, struct rusage *usage);

#ifdef __cplusplus
}
#endif

#endif
