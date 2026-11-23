/* Tiny image spawned with posix_spawn from the engine.
 *
 * The engine must not fork itself to arm PR_SET_PDEATHSIG: that copies
 * the live process's page tables under the audio callback. This program
 * is the child. It arms the signal (preserved across exec) and replaces
 * itself with the command it was given, so that command dies when the
 * engine does.
 */
#define _GNU_SOURCE
#include <signal.h>
#include <sys/prctl.h>
#include <unistd.h>

int main(int argc, char **argv) {
    pid_t parent;
    if (argc < 2) {
        return 2;
    }
    parent = getppid();
    if (prctl(PR_SET_PDEATHSIG, (unsigned long)SIGTERM) != 0) {
        return 3;
    }
    /* The parent exited before the signal was armed. */
    if (getppid() != parent) {
        raise(SIGTERM);
        return 4;
    }
    execvp(argv[1], &argv[1]);
    return 127;
}
