/* A PipeWire client whose audio thread busy-loops, the way cliamp's did.
 *
 * It makes a PipeWire context as any app does (client.conf loads
 * libpipewire-module-rt, which makes the data thread real-time through the
 * realtime group's limits or, failing that, the portal or rtkit), then runs
 * a busy loop of BUSY_MS (default 1000) of CPU on that data thread. It never
 * connects a stream, so it never joins the audio graph and can run next to
 * live audio.
 *
 * With rtkit's limit (RLIMIT_RTTIME 200000/200000 µs) the kernel kills the
 * process with SIGKILL after 200 ms: exit status 137. Otherwise it prints
 * the thread's scheduling and limit and exits 0.
 *
 * integration/test-rt-clients.py builds and runs it; by hand:
 *
 *   cc rt-busy-client.c $(pkg-config --cflags --libs libpipewire-0.3) -o rt-busy-client
 *   ./rt-busy-client                    # as an app in this session
 *   prlimit --rtprio=0 ./rt-busy-client # as an app before the re-login
 *                                       # (the realtime group's limits not yet in force)
 */
#define _GNU_SOURCE
#include <errno.h>
#include <string.h>
#include <pthread.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/resource.h>
#include <time.h>

#include <pipewire/pipewire.h>

static double thread_cpu_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_THREAD_CPUTIME_ID, &ts);
    return ts.tv_sec * 1e3 + ts.tv_nsec / 1e6;
}

static const char *policy_name(int p)
{
    switch (p & ~SCHED_RESET_ON_FORK) {
    case SCHED_OTHER: return "OTHER";
    case SCHED_FIFO: return "FIFO";
    case SCHED_RR: return "RR";
    default: return "?";
    }
}

static volatile unsigned long spins;

static int busy(struct spa_loop *loop, bool async, uint32_t seq,
                const void *data, size_t size, void *user)
{
    double ms = *(const double *)data;
    int policy = SCHED_OTHER;
    struct sched_param sp;
    struct rlimit rl;
    (void)loop; (void)async; (void)seq; (void)size; (void)user;
    /* rtkit makes the thread real-time a moment after it starts (module-rt
     * asks it over D-Bus from its own thread): wait up to 2 s for that,
     * sleeping, which blocks and costs no real-time CPU. */
    /* The kernel's answer: glibc's pthread_getschedparam() returns what was
     * last set through pthreads, not what rtkit set. */
    for (int i = 0; i < 40; i++) {
        policy = sched_getscheduler(0);
        sched_getparam(0, &sp);
        if ((policy & ~SCHED_RESET_ON_FORK) != SCHED_OTHER)
            break;
        struct timespec wait = { 0, 50 * 1000 * 1000 };
        nanosleep(&wait, NULL);
    }
    getrlimit(RLIMIT_RTTIME, &rl);
    printf("data thread %d: %s %d, RLIMIT_RTTIME %lld/%lld us\n", gettid(), policy_name(policy),
           sp.sched_priority,
           rl.rlim_cur == RLIM_INFINITY ? -1LL : (long long)rl.rlim_cur,
           rl.rlim_max == RLIM_INFINITY ? -1LL : (long long)rl.rlim_max);
    fflush(stdout);
    double end = thread_cpu_ms() + ms;
    while (thread_cpu_ms() < end)
        spins++;
    return 0;
}

int main(void)
{
    const char *env = getenv("BUSY_MS");
    double ms = env ? atof(env) : 1000.0;

    pw_init(NULL, NULL);
    struct pw_main_loop *main_loop = pw_main_loop_new(NULL);
    struct pw_context *context = pw_context_new(pw_main_loop_get_loop(main_loop),
        pw_properties_new(PW_KEY_APP_NAME, "rt-busy-client", NULL), 0);
    if (!context) {
        fprintf(stderr, "no PipeWire context: %s\n", strerror(errno));
        return 2;
    }
    /* The data loop the app's streams would run on, real-time as theirs is. */
    struct pw_data_loop *data = pw_context_get_data_loop(context);
    pw_data_loop_start(data);
    pw_loop_invoke(pw_data_loop_get_loop(data), busy, 0, &ms, sizeof(ms), true, NULL);
    printf("survived %.0f ms of busy audio thread\n", ms);

    pw_context_destroy(context);
    pw_main_loop_destroy(main_loop);
    pw_deinit();
    return 0;
}
