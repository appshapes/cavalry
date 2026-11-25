/* The C runner's closed round (work item 454; spec/c-abi.md, "cluster service"; ADR
 * 0155): a C program over crates/cavalry-ffi/include/cavalry.h that polls a cluster's
 * service whose driver stops, and checks that the poll of a closed client is one
 * answer, −1 with CAVALRY_ERR_CLOSED, at the first poll that fails and at every poll
 * after it, with the client's reason CAVALRY_CLOSED_DRIVER_CLOSED, and that the
 * service's close is then 0. The driver hosts no
 * member, so the service holds no log and its poll has no command to send: the case
 * in which a poll of a closed client took nothing and returned 0 for as long as it
 * was called. conformance/c/run.sh builds, links and runs it last, against the
 * driver its quickstart ran on; just conformance runs that.
 *
 *     closed-c <the driver's process id>
 *
 * The client attaches to the driver at CAVALRY_DIR, and the program stops that
 * driver itself, with SIGTERM to the process id it was given. A failed check prints
 * what failed and exits 1; the wait is bounded at WAIT_SECONDS. */
/* clock_gettime, nanosleep, kill and CLOCK_MONOTONIC are POSIX, not C11: glibc declares
 * them under -std=c11 only with this defined before the first include; macOS does as
 * well. */
#define _POSIX_C_SOURCE 200809L
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/types.h>
#include <time.h>

#include "cavalry.h"

#define WAIT_SECONDS 60
#define POLLS_AFTER 3

static int passed(const struct timespec *at) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec > at->tv_sec || (now.tv_sec == at->tv_sec && now.tv_nsec > at->tv_nsec);
}

static void fail(const char *function) {
    fprintf(stderr, "conformance/c: closed round: %s failed with code %d: %s\n", function,
            cavalry_errcode(), cavalry_errmsg());
    exit(1);
}

static void wrong(const char *what, int found) {
    fprintf(stderr, "conformance/c: closed round: %s: %d\n", what, found);
    exit(1);
}

#define CHECK(call) do { if ((call) != 0) fail(#call); } while (0)

/* No member, so no entry: a call is a failure of the round. */
static void on_entry(void *clientd, const uint8_t *buffer, size_t length, const cavalry_header_t *header) {
    (void) buffer;
    (void) length;
    (void) header;
    *(int *) clientd += 1;
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: closed-c <the driver's process id>\n");
        return 2;
    }
    long pid = strtol(argv[1], NULL, 10);
    if (pid <= 1) {
        fprintf(stderr, "closed-c: %s is no process id of a driver\n", argv[1]);
        return 2;
    }

    cavalry_context_t *context = NULL;
    cavalry_t *client = NULL;
    cavalry_cluster_service_t *service = NULL;
    int entries = 0;
    CHECK(cavalry_context_init(&context));
    CHECK(cavalry_init(&client, context));
    CHECK(cavalry_cluster_service(&service, client));

    /* 1. Beside a driver that runs and hosts no member, a poll takes nothing. */
    int taken = cavalry_cluster_service_poll(service, client, on_entry, &entries, 10);
    if (taken != 0) wrong("a poll beside a driver with no member did not return 0", taken);
    if (cavalry_is_closed(client)) wrong("the client is closed beside a driver that runs", cavalry_closed(client));

    /* 2. The driver stops. Until the client has seen it, a poll takes nothing; the
     * first poll that fails is the close, and nothing else. */
    if (kill((pid_t) pid, SIGTERM) != 0) wrong("the driver's process took no SIGTERM", (int) pid);
    struct timespec until;
    clock_gettime(CLOCK_MONOTONIC, &until);
    until.tv_sec += WAIT_SECONDS;
    int polls = 0;
    for (;;) {
        taken = cavalry_cluster_service_poll(service, client, on_entry, &entries, 10);
        polls++;
        if (taken < 0) break;
        if (taken != 0) wrong("a poll beside a driver with no member took entries", taken);
        if (passed(&until)) wrong("no poll failed within 60 s of the driver's stop; cavalry_closed says", cavalry_closed(client));
        struct timespec pause = {0, 1000000L};
        nanosleep(&pause, NULL);
    }
    if (cavalry_errcode() != CAVALRY_ERR_CLOSED)
        wrong("the first poll that failed after the driver's stop did not set CAVALRY_ERR_CLOSED", cavalry_errcode());
    cavalry_fault_t fault;
    CHECK(cavalry_cluster_service_fault(service, &fault));
    if (fault.code != CAVALRY_ERR_CLOSED) wrong("cavalry_cluster_service_fault did not give CAVALRY_ERR_CLOSED", fault.code);
    if (!cavalry_is_closed(client)) wrong("the poll said closed and cavalry_is_closed did not", 0);
    if (cavalry_closed(client) != CAVALRY_CLOSED_DRIVER_CLOSED)
        wrong("the client's reason is not CAVALRY_CLOSED_DRIVER_CLOSED", cavalry_closed(client));

    /* 3. Every poll after it is the same answer: a loop that polls ends. */
    for (int after = 1; after <= POLLS_AFTER; after++) {
        taken = cavalry_cluster_service_poll(service, client, on_entry, &entries, 10);
        if (taken != -1) wrong("a poll of a closed client did not return -1", taken);
        if (cavalry_errcode() != CAVALRY_ERR_CLOSED)
            wrong("a poll of a closed client did not set CAVALRY_ERR_CLOSED", cavalry_errcode());
    }
    if (entries != 0) wrong("the handler was called beside a driver with no member", entries);

    /* 4. The service closes with no error: a driver that is gone cannot be told. The
     * close of the client itself may say that its driver is gone, and its answer is
     * not this round's. */
    CHECK(cavalry_cluster_service_close(service, client));
    (void) cavalry_close(client);
    (void) cavalry_context_close(context);
    printf("conformance/c: closed round: a service beside a driver with no member took nothing; after the driver's "
           "stop, poll %d and the %d after it returned -1 with CAVALRY_ERR_CLOSED, the client's reason is "
           "CAVALRY_CLOSED_DRIVER_CLOSED, and the service closed with 0\n", polls, POLLS_AFTER);
    printf("conformance/c: closed round ok\n");
    return 0;
}
