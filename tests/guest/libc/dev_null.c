/* `/dev/null` opens for reading and writing: a read answers 0, a write of n
   bytes answers n, it is a character device that is no terminal, poll finds it
   ready both ways, its access mode holds, `fopen` opens it as `open` does,
   and a child given it as stdin and stdout by file actions, as Ninja and LLVM
   give it, reads end of file and has its write taken. */
#include <fcntl.h>
#include <poll.h>
#include <sys/stat.h>

#include "check.h"

#define SELF "/system/bin/test_c_dev_null"

static char block[4096];

static int child(void) {
    char byte;
    if (read(0, &byte, 1) != 0)
        return 2;
    if (write(1, block, sizeof block) != (ssize_t)sizeof block)
        return 3;
    return 0;
}

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "child") == 0)
        return child();
    memset(block, 'x', sizeof block);

    int fd = open("/dev/null", O_RDWR);
    CHECK(fd >= 0, "open O_RDWR");
    char buf[16];
    ssize_t n = read(fd, buf, sizeof buf);
    CHECK(n == 0, "read answered %zd", n);
    n = write(fd, block, sizeof block);
    CHECK(n == (ssize_t)sizeof block, "write answered %zd", n);
    struct stat st;
    CHECK(fstat(fd, &st) == 0, "fstat");
    CHECK(S_ISCHR(st.st_mode), "st_mode %#o", st.st_mode);
    CHECK(!isatty(fd), "isatty");
    struct pollfd p = {.fd = fd, .events = POLLIN | POLLOUT};
    int ready = poll(&p, 1, -1);
    CHECK(ready == 1 && (p.revents & POLLIN) && (p.revents & POLLOUT), "poll %d revents %#x", ready, p.revents);
    CHECK(close(fd) == 0, "close");

    int r = open("/dev/null", O_RDONLY);
    CHECK(r >= 0, "open O_RDONLY");
    CHECK(read(r, buf, sizeof buf) == 0, "read of O_RDONLY");
    CHECK(write(r, block, 1) < 0, "a write of an O_RDONLY /dev/null was taken");
    close(r);
    int w = open("/dev/null", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    CHECK(w >= 0, "open O_WRONLY|O_CREAT|O_TRUNC");
    CHECK(write(w, block, 1) == 1, "write of O_WRONLY");
    CHECK(read(w, buf, 1) < 0, "a read of an O_WRONLY /dev/null was answered");
    close(w);

    FILE *out = fopen("/dev/null", "w");
    CHECK(out != NULL, "fopen \"w\"");
    CHECK(fputs("taken\n", out) >= 0, "fputs");
    CHECK(fclose(out) == 0, "fclose of \"w\"");
    FILE *in = fopen("/dev/null", "r");
    CHECK(in != NULL, "fopen \"r\"");
    CHECK(fgetc(in) == EOF && feof(in), "fgetc of \"r\" was not end of file");
    CHECK(fclose(in) == 0, "fclose of \"r\"");

    posix_spawn_file_actions_t actions;
    CHECK(posix_spawn_file_actions_init(&actions) == 0, "refused");
    CHECK(posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0) == 0, "refused");
    CHECK(posix_spawn_file_actions_addopen(&actions, 1, "/dev/null", O_WRONLY, 0) == 0, "refused");
    int status = waited(spawn_role(SELF, "child", "", &actions, NULL));
    posix_spawn_file_actions_destroy(&actions);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "the child's status %#x: 2 for a read that was not end of file, 3 for a short write", status);
    printf("dev_null: /dev/null read end of file and took every write, here and as a child's stdio\n");
    return 0;
}
