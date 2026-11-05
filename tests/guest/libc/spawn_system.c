/* system and pclose answer a command's exit, run by /system/bin/shell -c,
   and popen its output; a command whose redirect does not open is not run. */
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

    /* A redirect that does not open runs nothing: no output, and a failure. */
    FILE *refused = popen("echo unsent > /tmp/no-such-directory/file", "r");
    CHECK(refused != NULL, "popen of a refused redirect");
    while (fgets(line, sizeof line, refused) != NULL)
        CHECK(strstr(line, "unsent") == NULL, "a refused redirect's command ran and said %s", line);
    status = pclose(refused);
    CHECK(!(WIFEXITED(status) && WEXITSTATUS(status) == 0), "a refused redirect answered %#x", status);
    printf("spawn_system: system and pclose answered the command's exit 3, popen its output, "
           "and a refused redirect ran nothing\n");
    return 0;
}
