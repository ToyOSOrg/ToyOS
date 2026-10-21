/* What setenv and unsetenv make of environ is the environment a child is
   spawned with: a variable set is there, and one removed that the process
   started with is not. The test starts a middle process with that variable,
   and the middle process starts the child. */
#include "check.h"

#define SELF "/system/bin/test_c_spawn_environ"

static void child(void) {
    const char *set = getenv("SPAWN_ENVIRON_SET");
    CHECK(set && strcmp(set, "here") == 0, "the child's SPAWN_ENVIRON_SET is %s", set ? set : "absent");
    CHECK(getenv("SPAWN_ENVIRON_GONE") == NULL, "the child still has SPAWN_ENVIRON_GONE");
}

static void middle(void) {
    CHECK(getenv("SPAWN_ENVIRON_GONE") != NULL, "the middle process started without SPAWN_ENVIRON_GONE");
    CHECK(setenv("SPAWN_ENVIRON_SET", "here", 1) == 0, "setenv");
    CHECK(unsetenv("SPAWN_ENVIRON_GONE") == 0, "unsetenv");
    CHECK(getenv("SPAWN_ENVIRON_GONE") == NULL, "unsetenv left SPAWN_ENVIRON_GONE");
    CHECK(setenv("SPAWN_ENVIRON_SET", "not this", 0) == 0 && strcmp(getenv("SPAWN_ENVIRON_SET"), "here") == 0,
          "setenv without overwrite replaced the value");
    errno = 0;
    CHECK(setenv("A=B", "x", 1) == -1 && errno == EINVAL, "setenv of a name with '='");
    int status = waited(spawn_role(SELF, "child", "", NULL, NULL));
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "the child's status %#x", status);
}

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "child") == 0) {
        child();
        return 0;
    }
    if (argc >= 2 && strcmp(argv[1], "middle") == 0) {
        middle();
        return 0;
    }
    size_t n = 0;
    while (environ[n])
        n++;
    char **envp = calloc(n + 2, sizeof *envp);
    CHECK(envp != NULL, "calloc");
    memcpy(envp, environ, n * sizeof *envp);
    envp[n] = "SPAWN_ENVIRON_GONE=1";
    char *args[] = {SELF, "middle", NULL};
    pid_t pid;
    int refused = posix_spawn(&pid, SELF, NULL, NULL, args, envp);
    CHECK(refused == 0, "posix_spawn of the middle process answered %d", refused);
    int status = waited(pid);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "the middle process's status %#x", status);
    printf("spawn_environ: setenv and unsetenv were seen in the child's environ\n");
    return 0;
}
