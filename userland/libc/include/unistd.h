#ifndef _UNISTD_H
#define _UNISTD_H

#include <stddef.h>
#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

#define STDIN_FILENO  0
#define STDOUT_FILENO 1
#define STDERR_FILENO 2

ssize_t read(int fd, void *buf, size_t count);
ssize_t write(int fd, const void *buf, size_t count);
int close(int fd);
off_t lseek(int fd, off_t offset, int whence);
int dup(int oldfd);
int dup2(int oldfd, int newfd);
int unlink(const char *path);
int rmdir(const char *path);
char *getcwd(char *buf, size_t size);
int chdir(const char *path);
int access(const char *path, int mode);
unsigned int sleep(unsigned int seconds);
int usleep(unsigned int usec);
int isatty(int fd);
int execvp(const char *file, char *const argv[]);
int execv(const char *path, char *const argv[]);
int execve(const char *path, char *const argv[], char *const envp[]);
int fork(void);
int pipe(int pipefd[2]);
void _exit(int status);
int fsync(int fd);
int ftruncate(int fd, off_t length);
ssize_t readlink(const char *path, char *buf, size_t size);
int symlink(const char *target, const char *linkpath);
int link(const char *existing, const char *newpath);
int fchown(int fd, uid_t owner, gid_t group);
unsigned int alarm(unsigned int seconds);
int gethostname(char *name, size_t len);

pid_t getpid(void);
pid_t getppid(void);
pid_t getsid(pid_t pid);
pid_t setsid(void);
uid_t getuid(void);
uid_t geteuid(void);
gid_t getgid(void);
gid_t getegid(void);
int kill(pid_t pid, int sig);

long sysconf(int name);

#define _SC_ARG_MAX          0
#define _SC_PAGESIZE        30
#define _SC_PAGE_SIZE       _SC_PAGESIZE
#define _SC_GETPW_R_SIZE_MAX 70
#define _SC_CLK_TCK          2
#define _SC_NPROCESSORS_ONLN 84

#define F_OK 0
#define R_OK 4
#define W_OK 2
#define X_OK 1

#ifdef __cplusplus
}
#endif

#endif
