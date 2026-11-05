/* A served file at stdout is given to the child system starts: the shell's
   output lands in the file after the parent's, at the description's offset. */
#include "check.h"
#include <fcntl.h>

#define PATH "/tmp/spawn_served_stdout"

int main(void) {
    int file = open(PATH, O_RDWR | O_CREAT | O_TRUNC, 0644);
    CHECK(file >= 0, "open %s", PATH);
    CHECK(write(file, "parent\n", 7) == 7, "write the parent's line");
    int saved = dup(1);
    CHECK(saved >= 0, "dup stdout");
    CHECK(dup2(file, 1) == 1, "dup2 the file to stdout");
    int status = system("echo child");
    CHECK(dup2(saved, 1) == 1, "restore stdout");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "system(\"echo child\") answered %#x", status);
    char said[32] = {0};
    ssize_t n = pread(file, said, sizeof said - 1, 0);
    CHECK(n == 13 && strcmp(said, "parent\nchild\n") == 0, "the file holds %zd bytes: %s", n, said);
    CHECK(unlink(PATH) == 0, "unlink %s", PATH);
    printf("spawn_served_stdout: system's child wrote to the served file at stdout\n");
    return 0;
}
