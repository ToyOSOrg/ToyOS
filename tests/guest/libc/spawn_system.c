/* system and pclose answer a command's exit, run by /system/bin/shell -c,
   and popen its output. */
#include "check.h"

int main(void) {
    int status = system("exit 3");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 3, "system(\"exit 3\") answered %#x", status);
    CHECK(system(NULL) != 0, "system(NULL) says there is no shell");

    FILE *said = popen("echo popen said", "r");
    CHECK(said != NULL, "popen");
    char line[64] = {0};
    CHECK(fgets(line, sizeof line, said) != NULL, "fgets from popen's stream");
    CHECK(strcmp(line, "popen said\n") == 0, "popen's stream said %s", line);
    status = pclose(said);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0, "pclose of echo answered %#x", status);

    FILE *ends = popen("exit 3", "r");
    CHECK(ends != NULL, "popen of exit 3");
    status = pclose(ends);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 3, "pclose of exit 3 answered %#x", status);
    printf("spawn_system: system and pclose answered the command's exit 3, and popen its output\n");
    return 0;
}
