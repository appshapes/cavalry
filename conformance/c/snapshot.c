/* The C runner's snapshot round (spec/c-abi.md, version 6, "A service's snapshot and its
 * start"; ADR 0146, ADR 0147, ADR 0149): a C program over
 * crates/cavalry-ffi/include/cavalry.h in which a service beside each of three cluster
 * members takes a snapshot of its program's state that its member lists, and a service
 * that starts again starts from it. conformance/c/run.sh builds, links and runs it after
 * the cluster round, beside the same members and on the log that round left; just
 * conformance runs that.
 *
 *     snapshot-c <cluster directory> <the cavalry binary>
 *
 * The members' drivers are <cluster directory>/member-0, -1 and -2. The round is the one
 * of conformance/README.md, "The snapshot round":
 *
 * 1. Each service asks where it starts, before its first poll:
 *    cavalry_cluster_service_restore answers CAVALRY_CLUSTER_RESTORED_FROM_START with a
 *    null state, and a second is refused with CAVALRY_ERR_CLUSTER_RESTORE_TOO_LATE.
 * 2. Each service reads an entry of the log. No snapshot is due, and
 *    cavalry_cluster_service_snapshot is refused with CAVALRY_ERR_CLUSTER_NO_SNAPSHOT_DUE.
 * 3. `cavalry snapshot` is run beside each member: beside the leader it exits 0 and prints
 *    the snapshot's log position, and beside a follower it exits 1 and names the leader.
 * 4. Each service's poll ends at the leader's request, handed over as
 *    CAVALRY_CLUSTER_ENTRY_SNAPSHOT, and cavalry_cluster_service_snapshot_due gives the
 *    position the command printed; the three services hold one state there. From inside
 *    the handler at the request, the three calls are refused with CAVALRY_ERR_POLLING, the
 *    read writes no position, and the refused restore writes a null state.
 * 5. Each service gives its state: the snapshot returns 1. None is due after, and a second
 *    is refused. The three services are then closed: one service beside a member takes
 *    snapshots.
 * 6. Beside each member a new service starts from the snapshot: the restore answers
 *    CAVALRY_CLUSTER_RESTORED_STATE and the state as it was given, which only a member
 *    that lists the snapshot names, and its position is the snapshot's.
 * 7. `cavalry snapshot` is run beside the leader again. Each new service's first entry is
 *    the request, at the first snapshot's position and no entry below it; its poll ends
 *    there, and it gives its state. A start beside each member is then given that state.
 * 8. A service that never asks where it starts reads the log from position 0. It meets the
 *    first request with the state the first services gave, and the second with the state
 *    the restarted ones hold. Its snapshot returns 0 both times.
 *
 * The program's state is how many entries it has read and a hash, FNV-1a over each entry's
 * type, position, term, time and payload, in order, the pair written out 155 times: 2,480
 * bytes, so that a snapshot holds it in three messages, the same beside every member. A
 * failed check prints what failed, with what the call returned, the thread's error code
 * and cavalry_errmsg where a call was to be refused, and exits 1. Every wait is bounded
 * at WAIT_SECONDS, `cavalry snapshot` among them: it is this program's own child, and
 * one that has not ended by then is killed by its process id. A snapshot that the driver
 * gave up at a step is given again. */
/* clock_gettime, nanosleep, fork, pipe and kill are POSIX, not C11: glibc declares them
 * under -std=c11 only with this defined before the first include; macOS does as well. */
#define _POSIX_C_SOURCE 200809L
#include <fcntl.h>
#include <inttypes.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include "cavalry.h"

#define MEMBERS 3
#define WAIT_SECONDS 60
#define COPIES 155
#define STATE_LENGTH (16 * COPIES)

static struct timespec after(long milliseconds) {
    struct timespec at;
    clock_gettime(CLOCK_MONOTONIC, &at);
    at.tv_sec += milliseconds / 1000;
    at.tv_nsec += (milliseconds % 1000) * 1000000L;
    if (at.tv_nsec >= 1000000000L) {
        at.tv_sec += 1;
        at.tv_nsec -= 1000000000L;
    }
    return at;
}

static int passed(const struct timespec *at) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec > at->tv_sec || (now.tv_sec == at->tv_sec && now.tv_nsec > at->tv_nsec);
}

static void idle(void) {
    struct timespec pause = { 0, 200000L };
    nanosleep(&pause, NULL);
}

static void fail(const char *function) {
    fprintf(stderr, "conformance/c: snapshot round: %s failed with code %d: %s\n", function,
            cavalry_errcode(), cavalry_errmsg());
    exit(1);
}

static void wrong(const char *what) {
    fprintf(stderr, "conformance/c: snapshot round: %s\n", what);
    exit(1);
}

#define CHECK(call) do { if ((call) != 0) fail(#call); } while (0)

/* A service beside one member and its program: the state is how many entries it has read
 * and a hash over them, which every member's service computes alike. */
typedef struct {
    int member;
    cavalry_context_t *context;
    cavalry_t *client;
    cavalry_cluster_service_t *service;
    uint64_t count;
    uint64_t hash;
    /* Of the entries taken since this service began: how many, the first one's type and
     * position, and the last one's type. */
    int taken;
    int32_t first_type, last_type;
    int64_t first_position;
    /* From inside the handler at the request: whether it was reached, which of
     * INSIDE_CHECKS did not hold, 0 for none, and the thread's error code then. */
    int inside, inside_failed, inside_code;
} program_t;

/* What the handler checks at the request, in order: the poll holds the service and its
 * client, so each of the three calls is refused with CAVALRY_ERR_POLLING. */
static const char *const INSIDE_CHECKS[] = {
    "",
    "the position of a null header did not answer -1 with CAVALRY_ERR_NULL",
    "cavalry_cluster_service_snapshot_due was not refused with CAVALRY_ERR_POLLING, or "
    "wrote a position",
    "cavalry_cluster_service_snapshot was not refused with CAVALRY_ERR_POLLING",
    "cavalry_cluster_service_restore was not refused with CAVALRY_ERR_POLLING, or wrote a "
    "state or a length",
};

static uint64_t fnv(uint64_t hash, const void *bytes, size_t length) {
    const uint8_t *at = bytes;
    for (size_t i = 0; i < length; i++) hash = (hash ^ at[i]) * 1099511628211ULL;
    return hash;
}

/* The program's state as bytes: the count and the hash, eight bytes each, in the host's
 * order, which is one order for the services of this process, COPIES times. */
static void state_of(const program_t *p, uint8_t state[STATE_LENGTH]) {
    for (int copy = 0; copy < COPIES; copy++) {
        memcpy(state + 16 * copy, &p->count, 8);
        memcpy(state + 16 * copy + 8, &p->hash, 8);
    }
}

/* Starts the program from `state`, as its snapshot held it. */
static void take(program_t *p, const uint8_t state[STATE_LENGTH]) {
    uint8_t again[STATE_LENGTH];
    memcpy(&p->count, state, 8);
    memcpy(&p->hash, state + 8, 8);
    state_of(p, again);
    if (memcmp(again, state, STATE_LENGTH) != 0) wrong("a state that is not one the program gave");
}

/* Sets the thread's error to a code no refusal of a poll gives, CAVALRY_ERR_NULL, by
 * asking for the position of a null header: a refusal checked after it set the error
 * itself. */
static int another_error(void) {
    return cavalry_committed_log_entry_position(NULL) == -1 &&
           cavalry_errcode() == CAVALRY_ERR_NULL;
}

static void on_entry(void *clientd, const uint8_t *buffer, size_t length,
                     const cavalry_header_t *header) {
    program_t *p = clientd;
    cavalry_cluster_entry_values_t values;
    if (cavalry_cluster_entry_values(header, &values) != 0) fail("cavalry_cluster_entry_values");
    p->count++;
    p->hash = fnv(p->hash, &values.type, sizeof values.type);
    p->hash = fnv(p->hash, &values.position, sizeof values.position);
    p->hash = fnv(p->hash, &values.leadership_term_id, sizeof values.leadership_term_id);
    p->hash = fnv(p->hash, &values.timestamp, sizeof values.timestamp);
    p->hash = fnv(p->hash, buffer, length);
    if (p->taken++ == 0) {
        p->first_type = values.type;
        p->first_position = values.position;
    }
    p->last_type = values.type;
    if (values.type == CAVALRY_CLUSTER_ENTRY_SNAPSHOT && !p->inside) {
        /* The poll holds the service and its client: each call is refused. The thread's
         * error is set to another code before each, so that it is that call that set it. */
        static uint8_t state[STATE_LENGTH];
        state_of(p, state);
        int64_t position = -7;
        const uint8_t *restored = state;
        size_t restored_length = sizeof state;
        int failed = 0;
        if (!another_error())
            failed = 1;
        else if (cavalry_cluster_service_snapshot_due(p->service, &position) ||
                 cavalry_errcode() != CAVALRY_ERR_POLLING || position != -7)
            failed = 2;
        else if (!another_error())
            failed = 1;
        else if (cavalry_cluster_service_snapshot(p->service, p->client, state, sizeof state) != -1 ||
                 cavalry_errcode() != CAVALRY_ERR_POLLING)
            failed = 3;
        else if (!another_error())
            failed = 1;
        else if (cavalry_cluster_service_restore(p->service, p->client, &restored,
                                                 &restored_length) != -1 ||
                 cavalry_errcode() != CAVALRY_ERR_POLLING || restored != NULL ||
                 restored_length != 0)
            failed = 4;
        p->inside = 1;
        p->inside_failed = failed;
        p->inside_code = cavalry_errcode();
    }
}

/* A call that was to be refused with `code` and was not: what it returned, and the
 * thread's error, which is an earlier call's when this one did not fail. */
static void not_refused(const char *call, int result, int code) {
    fprintf(stderr, "conformance/c: snapshot round: %s returned %d with the thread's error at "
                    "%d, not -1 with code %d: %s\n", call, result, cavalry_errcode(), code,
            cavalry_errmsg());
    exit(1);
}

/* A service beside member `member`, over a client on that member's driver. */
static void start(program_t *p, const char *cluster, int member) {
    char dir[1024];
    snprintf(dir, sizeof dir, "%s/member-%d", cluster, member);
    memset(p, 0, sizeof *p);
    p->member = member;
    p->hash = 14695981039346656037ULL;
    p->first_type = -1;
    p->first_position = -1;
    p->last_type = -1;
    CHECK(cavalry_context_init(&p->context));
    CHECK(cavalry_context_set_dir(p->context, dir));
    CHECK(cavalry_init(&p->client, p->context));
    CHECK(cavalry_cluster_service(&p->service, p->client));
}

static void stop(program_t *p) {
    CHECK(cavalry_cluster_service_close(p->service, p->client));
    CHECK(cavalry_close(p->client));
    CHECK(cavalry_context_close(p->context));
}

static int due(const program_t *p, int64_t *position) {
    int64_t at = -1;
    int is = cavalry_cluster_service_snapshot_due(p->service, &at);
    if (position != NULL) *position = at;
    return is;
}

static int poll(program_t *p) {
    int taken = cavalry_cluster_service_poll(p->service, p->client, on_entry, p, 16);
    if (taken < 0) fail("cavalry_cluster_service_poll");
    return taken;
}

/* Asks where the service starts until its driver hosts a member with a log: the restored
 * code, with the state's bytes copied to `state` for CAVALRY_CLUSTER_RESTORED_STATE. The
 * bytes are the handle's until the service's next poll. */
static int restore(program_t *p, uint8_t state[STATE_LENGTH]) {
    struct timespec until = after(WAIT_SECONDS * 1000L);
    for (;;) {
        const uint8_t *bytes = NULL;
        size_t length = 0;
        int restored = cavalry_cluster_service_restore(p->service, p->client, &bytes, &length);
        if (restored < 0) fail("cavalry_cluster_service_restore");
        if (restored != CAVALRY_CLUSTER_RESTORED_NO_MEMBER) {
            if (restored == CAVALRY_CLUSTER_RESTORED_STATE) {
                if (bytes == NULL || length != STATE_LENGTH)
                    wrong("a restored state that is not of the length the program gave");
                memcpy(state, bytes, STATE_LENGTH);
            } else if (bytes != NULL || length != 0) {
                wrong("a restore that gives no state wrote a pointer or a length");
            }
            return restored;
        }
        if (passed(&until)) wrong("a driver hosts no member with a log after 60 s");
        idle();
    }
}

/* A second restore of a service that has asked: refused, as too late. */
static void restore_is_too_late(program_t *p) {
    const uint8_t *bytes = NULL;
    size_t length = 0;
    int result = cavalry_cluster_service_restore(p->service, p->client, &bytes, &length);
    if (result != -1 || cavalry_errcode() != CAVALRY_ERR_CLUSTER_RESTORE_TOO_LATE)
        not_refused("a second cavalry_cluster_service_restore", result,
                    CAVALRY_ERR_CLUSTER_RESTORE_TOO_LATE);
}

/* Gives the program's state to the service's snapshot: 1 written, 0 already listed. A
 * snapshot that a step of the driver's gave up is still due, and is given again. */
static int give(program_t *p) {
    static uint8_t state[STATE_LENGTH];
    struct timespec until = after(WAIT_SECONDS * 1000L);
    state_of(p, state);
    for (;;) {
        int written = cavalry_cluster_service_snapshot(p->service, p->client, state, sizeof state);
        if (written >= 0) return written;
        if (cavalry_errcode() != CAVALRY_ERR_CLUSTER_SNAPSHOT_FAILED || passed(&until))
            fail("cavalry_cluster_service_snapshot");
        idle();
    }
}

/* A snapshot where none is due: refused. */
static void none_is_due(program_t *p) {
    static uint8_t state[STATE_LENGTH];
    state_of(p, state);
    int result = cavalry_cluster_service_snapshot(p->service, p->client, state, sizeof state);
    if (result != -1 || cavalry_errcode() != CAVALRY_ERR_CLUSTER_NO_SNAPSHOT_DUE)
        not_refused("cavalry_cluster_service_snapshot where none is due", result,
                    CAVALRY_ERR_CLUSTER_NO_SNAPSHOT_DUE);
}

/* Polls every service that has no snapshot to give until each has read an entry
 * (`request` 0) or has a snapshot due (`request` 1). */
static void read_on(program_t *programs, int count, int request, const char *what) {
    struct timespec until = after(WAIT_SECONDS * 1000L);
    for (;;) {
        int waiting = 0;
        for (int i = 0; i < count; i++) {
            if (due(&programs[i], NULL)) continue;
            if (request || programs[i].count == 0) {
                waiting++;
                poll(&programs[i]);
            }
        }
        if (waiting == 0) return;
        if (passed(&until)) wrong(what);
        idle();
    }
}

#define SAID 2048

/* What `cavalry snapshot` did beside one member: its exit code, and what it printed on
 * its standard output and on its standard error. */
typedef struct {
    int exit_code;
    char out[SAID], err[SAID];
} ran_t;

/* Reads what a pipe holds now into `said`, after `*length` bytes, and never past it. */
static void drain(int fd, char *said, size_t *length) {
    ssize_t count;
    while (*length < SAID - 1 && (count = read(fd, said + *length, SAID - 1 - *length)) > 0)
        *length += (size_t)count;
    said[*length] = '\0';
}

/* Runs `cavalry snapshot` beside member `member`, as this program's own child. A command
 * that has not ended in WAIT_SECONDS is killed, by its process id, and fails the round.
 * The command prints a line or two, far under a pipe's size and under SAID. */
static void ask(const char *binary, const char *cluster, int member, ran_t *ran) {
    char dir[1024];
    int outs[2], errs[2], status = 0;
    size_t out_length = 0, err_length = 0;
    snprintf(dir, sizeof dir, "%s/member-%d", cluster, member);
    ran->out[0] = '\0';
    ran->err[0] = '\0';
    if (pipe(outs) != 0 || pipe(errs) != 0) wrong("no pipe for `cavalry snapshot`");
    /* Set before the fork, so that no check between the fork and the wait can leave the
     * child running. */
    if (fcntl(outs[0], F_SETFL, O_NONBLOCK) != 0 || fcntl(errs[0], F_SETFL, O_NONBLOCK) != 0)
        wrong("the pipes of `cavalry snapshot` could not be read without waiting");
    pid_t child = fork();
    if (child < 0) wrong("`cavalry snapshot` could not be started");
    if (child == 0) {
        dup2(outs[1], STDOUT_FILENO);
        dup2(errs[1], STDERR_FILENO);
        close(outs[0]);
        close(outs[1]);
        close(errs[0]);
        close(errs[1]);
        execl(binary, binary, "snapshot", "--dir", dir, (char *)NULL);
        _exit(127);
    }
    close(outs[1]);
    close(errs[1]);
    struct timespec until = after(WAIT_SECONDS * 1000L);
    for (;;) {
        pid_t ended = waitpid(child, &status, WNOHANG);
        drain(outs[0], ran->out, &out_length);
        drain(errs[0], ran->err, &err_length);
        if (ended == child) break;
        if (ended < 0) {
            kill(child, SIGKILL);
            wrong("`cavalry snapshot` could not be waited for");
        }
        if (passed(&until)) {
            kill(child, SIGKILL);
            waitpid(child, &status, 0);
            fprintf(stderr, "conformance/c: snapshot round: `cavalry snapshot` beside member %d "
                            "did not end in %d s; it printed: %s%s\n", member, WAIT_SECONDS,
                    ran->out, ran->err);
            exit(1);
        }
        idle();
    }
    close(outs[0]);
    close(errs[0]);
    if (!WIFEXITED(status) || WEXITSTATUS(status) == 127) {
        fprintf(stderr, "conformance/c: snapshot round: `cavalry snapshot` beside member %d did "
                        "not run to its exit: %s is not a program that runs, or a signal ended "
                        "it; it printed: %s%s\n", member, binary, ran->out, ran->err);
        exit(1);
    }
    ran->exit_code = WEXITSTATUS(status);
}

/* The log position `cavalry snapshot` printed beside member `member`, with exit 0. */
static int64_t position_of(const ran_t *ran, int member) {
    int named = -1;
    int64_t position = -1;
    if (ran->exit_code != 0 ||
        sscanf(ran->out, "member %d lists a snapshot of log position %" SCNd64, &named,
               &position) != 2 || named != member) {
        fprintf(stderr, "conformance/c: snapshot round: `cavalry snapshot` beside member %d, exit "
                        "%d: %s%s\n", member, ran->exit_code, ran->out, ran->err);
        exit(1);
    }
    return position;
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: snapshot-c <cluster directory> <the cavalry binary>\n");
        return 2;
    }
    const char *cluster = argv[1], *binary = argv[2];
    static uint8_t state[STATE_LENGTH], given[STATE_LENGTH], next[STATE_LENGTH];

    /* 1. Where each service starts: no snapshot of a service is listed yet. */
    program_t programs[MEMBERS];
    for (int i = 0; i < MEMBERS; i++) {
        start(&programs[i], cluster, i);
        if (due(&programs[i], NULL)) wrong("a snapshot is due before a service's first poll");
        if (restore(&programs[i], state) != CAVALRY_CLUSTER_RESTORED_FROM_START)
            wrong("a service does not start from the log's start though no snapshot is listed");
        restore_is_too_late(&programs[i]);
    }

    /* 2. Every service reads an entry of the log the cluster round left; none is due. */
    read_on(programs, MEMBERS, 0, "a service read no entry of its member's log in 60 s");
    for (int i = 0; i < MEMBERS; i++) none_is_due(&programs[i]);

    /* 3. `cavalry snapshot` beside each member: the leader's is taken, a follower's is
     * refused and names the leader. */
    static ran_t asked[MEMBERS];
    int leader = -1;
    for (int i = 0; i < MEMBERS; i++) {
        ask(binary, cluster, i, &asked[i]);
        if (asked[i].exit_code == 0) {
            if (leader >= 0) wrong("`cavalry snapshot` exited 0 beside two members");
            leader = i;
        }
    }
    if (leader < 0) wrong("`cavalry snapshot` exited 0 beside no member");
    int64_t position = position_of(&asked[leader], leader);
    char leads[32];
    snprintf(leads, sizeof leads, "member %d leads", leader);
    for (int i = 0; i < MEMBERS; i++) {
        if (i != leader && (asked[i].exit_code != 1 || strstr(asked[i].err, leads) == NULL)) {
            fprintf(stderr, "conformance/c: snapshot round: `cavalry snapshot` beside member %d, a "
                            "follower, exit %d: %s%s\n", i, asked[i].exit_code, asked[i].out,
                    asked[i].err);
            return 1;
        }
    }

    /* 4. Every service's poll ends at the request, and the snapshot is due at the position
     * the command printed, with one state beside the three members. */
    read_on(programs, MEMBERS, 1, "a service's poll did not end at the leader's request in 60 s");
    state_of(&programs[0], given);
    for (int i = 0; i < MEMBERS; i++) {
        int64_t at = -1;
        if (!due(&programs[i], &at) || at != position ||
            cavalry_cluster_service_position(programs[i].service) != position ||
            programs[i].last_type != CAVALRY_CLUSTER_ENTRY_SNAPSHOT)
            wrong("a service's snapshot is not due at the position the command printed, after "
                  "the leader's request as its last entry");
        if (!programs[i].inside) wrong("a service's handler was not handed the leader's request");
        if (programs[i].inside_failed) {
            fprintf(stderr, "conformance/c: snapshot round: from inside the handler at the "
                            "request, beside member %d: %s; the thread's error was %d\n", i,
                    INSIDE_CHECKS[programs[i].inside_failed], programs[i].inside_code);
            return 1;
        }
        state_of(&programs[i], state);
        if (memcmp(state, given, STATE_LENGTH) != 0)
            wrong("the three services hold different states at the snapshot's position");
    }

    /* 5. Each service gives its state, once, and is closed: the service that starts from
     * the snapshot is the one that takes the next. */
    for (int i = 0; i < MEMBERS; i++) {
        if (give(&programs[i]) != 1) wrong("a service gave its state and the snapshot did not return 1");
        if (due(&programs[i], NULL)) wrong("a snapshot is still due after it was taken");
        none_is_due(&programs[i]);
        stop(&programs[i]);
    }

    /* 6. The member lists it: a new service beside each member starts from it. A member
     * lists the snapshot when it reads the service's word of it, a duty cycle or so later,
     * so a start that finds none is made again with a new service. */
    program_t restarted[MEMBERS];
    for (int i = 0; i < MEMBERS; i++) {
        struct timespec until = after(WAIT_SECONDS * 1000L);
        for (;;) {
            start(&restarted[i], cluster, i);
            if (restore(&restarted[i], state) == CAVALRY_CLUSTER_RESTORED_STATE) break;
            stop(&restarted[i]);
            if (passed(&until)) wrong("a member lists no snapshot of its service after 60 s");
            idle();
        }
        if (memcmp(state, given, STATE_LENGTH) != 0)
            wrong("a service starts from bytes that are not the state given");
        take(&restarted[i], state);
        if (cavalry_cluster_service_position(restarted[i].service) != position)
            wrong("a service that started from its snapshot is not at the snapshot's position");
        restore_is_too_late(&restarted[i]);
    }

    /* 7. A second request, beside the leader: each restarted service's first entry is the
     * request, at the first snapshot's position, and it gives its state. */
    ask(binary, cluster, leader, &asked[leader]);
    int64_t second = position_of(&asked[leader], leader);
    if (second <= position) wrong("a second snapshot that is not past the first");
    read_on(restarted, MEMBERS, 1, "a restarted service's poll did not end at the second request in 60 s");
    state_of(&restarted[0], next);
    if (memcmp(next, given, STATE_LENGTH) == 0)
        wrong("the restarted services hold the first state at the second snapshot's position");
    for (int i = 0; i < MEMBERS; i++) {
        int64_t at = -1;
        if (restarted[i].taken != 1 || restarted[i].first_type != CAVALRY_CLUSTER_ENTRY_SNAPSHOT ||
            restarted[i].first_position != position || !due(&restarted[i], &at) || at != second)
            wrong("a service that started from its snapshot was not handed the second request "
                  "as its one entry, at the first snapshot's position, with a snapshot due at "
                  "the second's");
        state_of(&restarted[i], state);
        if (memcmp(state, next, STATE_LENGTH) != 0)
            wrong("the restarted services hold different states at the second snapshot's position");
        if (give(&restarted[i]) != 1)
            wrong("a restarted service gave its state and the snapshot did not return 1");
    }
    /* Each member lists the second snapshot too: a start beside it is given the new state,
     * once the member has read the service's word of it. */
    for (int i = 0; i < MEMBERS; i++) {
        struct timespec until = after(WAIT_SECONDS * 1000L);
        for (;;) {
            program_t probe;
            start(&probe, cluster, i);
            int restored = restore(&probe, state);
            int64_t at = cavalry_cluster_service_position(probe.service);
            stop(&probe);
            if (restored == CAVALRY_CLUSTER_RESTORED_STATE && at == second &&
                memcmp(state, next, STATE_LENGTH) == 0)
                break;
            if (passed(&until)) wrong("a member does not list its service's second snapshot after 60 s");
            idle();
        }
    }

    /* 8. A service that never asks reads from position 0 and meets both requests, each
     * with the state the others held there: a start from a snapshot and a read from the
     * start reach one state. Its member lists a snapshot at each, or past it. */
    program_t late;
    start(&late, cluster, leader);
    for (int request = 0; request < 2; request++) {
        int64_t at = -1;
        read_on(&late, 1, 1, "a service that reads from the start did not meet a request in 60 s");
        state_of(&late, state);
        if (!due(&late, &at) || at != (request == 0 ? position : second) ||
            memcmp(state, request == 0 ? given : next, STATE_LENGTH) != 0)
            wrong("a service that read from the start is not due at a snapshot's position with "
                  "the state the services held there");
        if (give(&late) != 0)
            wrong("a snapshot of a position the member already lists or is past did not return 0");
    }

    for (int i = 0; i < MEMBERS; i++) stop(&restarted[i]);
    stop(&late);
    printf("conformance/c: snapshot round: three services gave a state of %d bytes at log position "
           "%" PRId64 ", asked for by `cavalry snapshot` beside member %d; each member listed its "
           "service's snapshot, a new service beside each started from it, read the next request "
           "at %" PRId64 " and gave its state at %" PRId64 "\n",
           STATE_LENGTH, position, leader, position, second);
    printf("conformance/c: snapshot round ok\n");
    return 0;
}
