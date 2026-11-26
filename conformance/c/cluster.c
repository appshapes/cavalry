/* The C runner's cluster round (work item 229; spec/c-abi.md, versions 4, 5 and 7;
 * ADR 0122, ADR 0123, ADR 0157): a C program over crates/cavalry-ffi/include/cavalry.h
 * that runs a cluster's service beside each of three members and holds a session
 * with the cluster, and checks that one message is handed to all three services as
 * the same entry and answered once, by the service beside the leader, with the
 * entry's term and the cluster's time; and that the timer each service's program
 * schedules at that message is handed to all three once, with its id, at one
 * position and no earlier than its deadline, and that a second timer, due at once and
 * cancelled in the same handling, is handed to none. conformance/c/run.sh starts the
 * three members, builds, links and runs it; just conformance runs that.
 *
 *     cluster-c <cluster directory> <members string> <response channel>
 *
 * The members' drivers are <cluster directory>/member-0, -1 and -2, and the session's
 * client attaches to the driver at CAVALRY_DIR. A failed check prints what failed,
 * with cavalry_errmsg when a call set the thread's error, and exits 1; every wait is
 * bounded at WAIT_SECONDS. */
/* clock_gettime, nanosleep and CLOCK_MONOTONIC are POSIX, not C11: glibc declares them
 * under -std=c11 only with this defined before the first include; macOS does as well. */
#define _POSIX_C_SOURCE 200809L
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "cavalry.h"

#define MEMBERS 3
#define WAIT_SECONDS 60
#define MESSAGE "hello"
#define MESSAGE_LENGTH 5
/* The timer each service's program schedules when it takes the message: its id, and
 * how long after the message's entry it is due, in the cluster's time. A second, and
 * not the 50 ms this round had first: the event of a timer that is due at once is
 * stamped tens of milliseconds after its message, the time an entry takes to be
 * committed and read by a service on a laptop, 11 to 42 ms in the Python round's runs
 * there, and more on a busy host, so a deadline of 50 ms is a thin margin for telling a
 * timer that was wrongly due at once from one that waited. Before ADR 0168 a service's
 * first timer waited for its publication to its member too, 49 ms in a run of the
 * Python round. */
#define TIMER_ID 7
#define TIMER_AFTER_MS 1000
/* The timer each service's program schedules as due at once and cancels in the same
 * handling: were the cancel to miss, it would be handed over, and before the first. */
#define CANCELLED_ID 8

/* A bound on a wait, `milliseconds` from now. */
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

static void fail(const char *function) {
    fprintf(stderr, "conformance/c: cluster round: %s failed with code %d: %s\n", function,
            cavalry_errcode(), cavalry_errmsg());
    exit(1);
}

static void wrong(const char *what) {
    fprintf(stderr, "conformance/c: cluster round: %s\n", what);
    exit(1);
}

#define CHECK(call) do { if ((call) != 0) fail(#call); } while (0)

/* A service beside one member: its handles, what its handler was handed, and what the
 * calls its handler made answered. */
typedef struct {
    int member;
    cavalry_context_t *context;
    cavalry_t *client;
    cavalry_cluster_service_t *service;
    int entries;                            /* entries handed over */
    int messages;                           /* a session's messages taken */
    int reply;                              /* what the reply to the last one answered */
    cavalry_cluster_entry_values_t message; /* the last one's values */
    uint8_t payload[64];
    size_t payload_length;
    /* From inside the handler: a service's header carries no frame and no reply's
     * values, it carries its entry's position, and the poll holds its client. */
    int no_frame, no_reply_values, one_position, client_held;
    /* The program's timers (spec/c-abi.md, version 7; ADR 0157): what the schedule
     * answered, what the second timer's schedule and its cancel answered, how many
     * timers the handler was handed, and the last one's values. */
    int scheduled;
    int cancelled_scheduled, cancelled;
    int timers;
    cavalry_cluster_entry_values_t timer;
} served_t;

/* The services' handler. It answers a session's message with its member's id, a colon
 * and the message, so that the reply a client reads says whose service sent it. */
static void on_entry(void *clientd, const uint8_t *buffer, size_t length,
                     const cavalry_header_t *header) {
    served_t *s = clientd;
    cavalry_cluster_entry_values_t values;
    if (cavalry_cluster_entry_values(header, &values) != 0) fail("cavalry_cluster_entry_values");
    s->entries++;
    if (values.type == CAVALRY_CLUSTER_ENTRY_TIMER) {
        s->timers++;
        s->timer = values;
        return;
    }
    if (values.type != CAVALRY_CLUSTER_ENTRY_SESSION_MESSAGE || values.session_id < 0) return;
    if (length > sizeof s->payload) wrong("a session message longer than this round sends");
    uint8_t answer[sizeof s->payload + 2];
    answer[0] = (uint8_t) ('0' + s->member);
    answer[1] = ':';
    memcpy(answer + 2, buffer, length);
    /* Reply first, then change the state: an entry that is aborted is handed over again. */
    int reply = cavalry_cluster_service_reply(s->service, values.session_id, answer, length + 2);
    if (reply < 0) fail("cavalry_cluster_service_reply");
    if (reply == CAVALRY_CLUSTER_REPLY_RETRY) {
        CHECK(cavalry_cluster_service_action(s->service, CAVALRY_ACTION_ABORT));
        return;
    }
    s->reply = reply;
    s->message = values;
    memcpy(s->payload, buffer, length);
    s->payload_length = length;
    s->messages++;
    cavalry_header_values_t frame;
    cavalry_cluster_reply_values_t reply_values;
    s->no_frame = cavalry_header_values(header, &frame) == -1 &&
                  cavalry_errcode() == CAVALRY_ERR_WRONG_HEADER;
    s->no_reply_values = cavalry_cluster_reply_values(header, &reply_values) == -1 &&
                         cavalry_errcode() == CAVALRY_ERR_WRONG_HEADER;
    s->one_position = cavalry_committed_log_entry_position(header) == values.position;
    s->client_held = !cavalry_is_closed(s->client) && cavalry_errcode() == CAVALRY_ERR_POLLING;
    /* The program's state changes at the entry it took: a timer, due a little after
     * it. Every member's service makes the same call at the same entry. */
    s->scheduled = cavalry_cluster_service_schedule_timer(s->service, TIMER_ID,
                                                          values.timestamp + TIMER_AFTER_MS);
    if (s->scheduled < 0) fail("cavalry_cluster_service_schedule_timer");
    s->cancelled_scheduled = cavalry_cluster_service_schedule_timer(s->service, CANCELLED_ID, values.timestamp);
    if (s->cancelled_scheduled < 0) fail("cavalry_cluster_service_schedule_timer");
    s->cancelled = cavalry_cluster_service_cancel_timer(s->service, CANCELLED_ID);
    if (s->cancelled < 0) fail("cavalry_cluster_service_cancel_timer");
}

/* What the session's handler was handed. */
typedef struct {
    int replies;
    uint8_t payload[sizeof(((served_t *) 0)->payload) + 2];
    size_t length;
    cavalry_cluster_reply_values_t values;
} answered_t;

static void on_reply(void *clientd, const uint8_t *buffer, size_t length,
                     const cavalry_header_t *header) {
    answered_t *a = clientd;
    if (cavalry_cluster_reply_values(header, &a->values) != 0) fail("cavalry_cluster_reply_values");
    if (length > sizeof a->payload) wrong("a reply longer than any this round's services send");
    memcpy(a->payload, buffer, length);
    a->length = length;
    a->replies++;
}

/* One duty cycle: every service's poll, the session's when there is one, and a short
 * sleep, since the members run in processes of their own. */
static void cycle(served_t *served, cavalry_cluster_session_t *session, answered_t *answered) {
    for (int i = 0; i < MEMBERS; i++) {
        if (cavalry_cluster_service_poll(served[i].service, served[i].client, on_entry, &served[i],
                                         16) < 0)
            fail("cavalry_cluster_service_poll");
    }
    if (session != NULL && cavalry_cluster_session_poll(session, on_reply, answered, 16) < 0)
        fail("cavalry_cluster_session_poll");
    struct timespec pause = { 0, 200000L };
    nanosleep(&pause, NULL);
}

int main(int argc, char **argv) {
    if (argc != 4) {
        fprintf(stderr, "usage: cluster-c <cluster directory> <members string> <response channel>\n");
        return 2;
    }
    const char *cluster = argv[1], *members = argv[2], *response_channel = argv[3];

    /* 1. A service beside each member, over a client on that member's driver. */
    served_t served[MEMBERS];
    memset(served, 0, sizeof served);
    for (int i = 0; i < MEMBERS; i++) {
        char dir[1024];
        snprintf(dir, sizeof dir, "%s/member-%d", cluster, i);
        served[i].member = i;
        CHECK(cavalry_context_init(&served[i].context));
        CHECK(cavalry_context_set_dir(served[i].context, dir));
        CHECK(cavalry_init(&served[i].client, served[i].context));
        CHECK(cavalry_cluster_service(&served[i].service, served[i].client));
    }
    /* The handler's calls are for the handler: from here there is no entry. */
    if (cavalry_cluster_service_reply(served[0].service, 1, (const uint8_t *) "x", 1) != -1 ||
        cavalry_errcode() != CAVALRY_ERR_CLUSTER_NO_ENTRY)
        wrong("a reply from outside the handler was not refused with CAVALRY_ERR_CLUSTER_NO_ENTRY");

    /* 2. Each service opens its member's log and reads the term's first entry before
     * the session connects: a service answers no entry that its member held when the
     * service first opened the log (spec/cluster.md, "Replies"). */
    struct timespec until = after(WAIT_SECONDS * 1000L);
    while (served[0].entries == 0 || served[1].entries == 0 || served[2].entries == 0) {
        if (passed(&until)) wrong("a service read no entry of its member's log in 60 s");
        cycle(served, NULL, NULL);
    }

    /* 3. A session with the cluster, on the driver at CAVALRY_DIR. The connect returns
     * once the leader has opened the session. */
    cavalry_context_t *context = NULL;
    cavalry_t *client = NULL;
    cavalry_cluster_session_t *session = NULL;
    CHECK(cavalry_context_init(&context));
    CHECK(cavalry_init(&client, context));
    CHECK(cavalry_connect_cluster(&session, client, members, response_channel));
    cavalry_cluster_session_constants_t constants;
    CHECK(cavalry_cluster_session_constants(session, &constants));

    /* 4. One message, taken by every service and answered by one. */
    answered_t answered;
    memset(&answered, 0, sizeof answered);
    until = after(WAIT_SECONDS * 1000L);
    for (;;) {
        int64_t offered =
            cavalry_cluster_session_offer(session, (const uint8_t *) MESSAGE, MESSAGE_LENGTH);
        if (offered >= 0) break;
        if (offered == CAVALRY_PUBLICATION_ERROR) fail("cavalry_cluster_session_offer");
        if (offered == CAVALRY_PUBLICATION_CLOSED) wrong("the session closed before its message");
        if (passed(&until)) wrong("the session's publication took no message in 60 s");
        cycle(served, session, &answered);
    }
    while (answered.replies == 0 || served[0].messages == 0 || served[1].messages == 0 ||
           served[2].messages == 0) {
        if (passed(&until)) wrong("the message was not taken by three services and answered in 60 s");
        cycle(served, session, &answered);
    }
    /* A second reply would come in this time. */
    until = after(300);
    while (!passed(&until)) cycle(served, session, &answered);
    if (answered.replies != 1) wrong("the session read more than one reply to one message");

    /* 5. The same entry beside every member, answered by the leader's service alone. */
    int leader = -1;
    for (int i = 0; i < MEMBERS; i++) {
        const served_t *s = &served[i];
        if (s->messages != 1) wrong("a service took the message more than once");
        if (s->message.position != served[0].message.position ||
            s->message.leadership_term_id != served[0].message.leadership_term_id ||
            s->message.timestamp != served[0].message.timestamp ||
            s->message.session_id != constants.session_id)
            wrong("the services were handed different values for one entry");
        if (s->payload_length != MESSAGE_LENGTH || memcmp(s->payload, MESSAGE, MESSAGE_LENGTH) != 0)
            wrong("a service was handed another payload than the session sent");
        if (!s->no_frame || !s->no_reply_values || !s->one_position)
            wrong("a service's header did not answer as \"What a header carries\" says");
        if (!s->client_held)
            wrong("a call on the poll's client from inside the handler was not refused with "
                  "CAVALRY_ERR_POLLING");
        if (cavalry_cluster_service_sessions(s->service) != 1)
            wrong("a service does not hold one session open");
        if (cavalry_cluster_service_position(s->service) <= s->message.position)
            wrong("a service's position is not past the entry it took");
        if (s->reply == CAVALRY_CLUSTER_REPLY_SENT) {
            if (leader != -1) wrong("two services answered one entry");
            if (!cavalry_cluster_service_answers(s->service))
                wrong("the service that answered says it does not answer");
            leader = i;
        } else if (s->reply != CAVALRY_CLUSTER_REPLY_NOT_ANSWERING ||
                   cavalry_cluster_service_answers(s->service)) {
            wrong("a service beside a follower did not answer CAVALRY_CLUSTER_REPLY_NOT_ANSWERING");
        }
    }
    if (leader == -1) wrong("no service answered the entry");
    if (answered.length != MESSAGE_LENGTH + 2 || answered.payload[0] != (uint8_t) ('0' + leader) ||
        answered.payload[1] != ':' || memcmp(answered.payload + 2, MESSAGE, MESSAGE_LENGTH) != 0)
        wrong("the reply is not the leader's service's");
    if (answered.values.leadership_term_id != served[leader].message.leadership_term_id ||
        answered.values.timestamp != served[leader].message.timestamp)
        wrong("the reply does not carry its entry's term and the cluster's time");
    printf("conformance/c: session %" PRId64 "'s message, the entry at position %" PRId64
           " of term %" PRId64 ", taken by three services and answered once, by the service "
           "beside member %d\n",
           constants.session_id, served[leader].message.position,
           served[leader].message.leadership_term_id, leader);

    /* 5b. The timer that each service's program scheduled at the message is handed to
     * every service once, as an entry of its own kind with the timer's id, at one
     * position, and at no time before its deadline (spec/c-abi.md, version 7;
     * spec/cluster.md, "The service", "Timers"; ADR 0157). */
    until = after(WAIT_SECONDS * 1000L);
    while (served[0].timers == 0 || served[1].timers == 0 || served[2].timers == 0) {
        if (passed(&until)) wrong("the timer was not handed to three services in 60 s");
        cycle(served, session, &answered);
    }
    /* A second handing of the same event would come in this time. A second event,
     * which a refresh of the leader's member can make a second later, is passed over
     * by every service, the timer being pending no more. */
    until = after(300);
    while (!passed(&until)) cycle(served, session, &answered);
    for (int i = 0; i < MEMBERS; i++) {
        const served_t *s = &served[i];
        if (s->scheduled != CAVALRY_CLUSTER_TIMER_SCHEDULED)
            wrong("a service's schedule of its timer was not answered CAVALRY_CLUSTER_TIMER_SCHEDULED");
        if (s->cancelled_scheduled != CAVALRY_CLUSTER_TIMER_SCHEDULED || s->cancelled != 0)
            wrong("a service's schedule of its second timer was not answered CAVALRY_CLUSTER_TIMER_SCHEDULED, "
                  "or its cancel 0");
        if (s->timers != 1)
            wrong("a service was handed more than one timer: its timer twice, or the one it cancelled");
        if (s->timer.correlation_id != TIMER_ID) wrong("a service was handed another timer's id");
        if (s->timer.position != served[0].timer.position ||
            s->timer.timestamp != served[0].timer.timestamp)
            wrong("the services were handed the timer at different positions or times");
        if (s->timer.timestamp < s->message.timestamp + TIMER_AFTER_MS)
            wrong("a service was handed the timer before its deadline");
        if (s->timer.position <= s->message.position)
            wrong("a service was handed the timer at or before the entry that scheduled it");
    }
    if (cavalry_cluster_service_schedule_timer(served[0].service, TIMER_ID, 0) != -1 ||
        cavalry_errcode() != CAVALRY_ERR_CLUSTER_NO_ENTRY)
        wrong("a schedule of a timer from outside the handler was not refused with "
              "CAVALRY_ERR_CLUSTER_NO_ENTRY");
    if (cavalry_cluster_service_cancel_timer(served[0].service, TIMER_ID) != -1 ||
        cavalry_errcode() != CAVALRY_ERR_CLUSTER_NO_ENTRY)
        wrong("a cancel of a timer from outside the handler was not refused with "
              "CAVALRY_ERR_CLUSTER_NO_ENTRY");
    printf("conformance/c: timer %d, scheduled by each service at the message for %d ms later, "
           "handed to three services once, at position %" PRId64 ", %" PRId64 " ms after the message; "
           "timer %d, due at once and cancelled in the same handling, handed to none\n",
           TIMER_ID, TIMER_AFTER_MS, served[0].timer.position,
           served[0].timer.timestamp - served[0].message.timestamp, CANCELLED_ID);

    /* 6. The session before its client, and each service before its own. */
    CHECK(cavalry_cluster_session_close(session));
    CHECK(cavalry_close(client));
    CHECK(cavalry_context_close(context));
    for (int i = 0; i < MEMBERS; i++) {
        CHECK(cavalry_cluster_service_close(served[i].service, served[i].client));
        CHECK(cavalry_close(served[i].client));
        CHECK(cavalry_context_close(served[i].context));
    }
    printf("conformance/c: cluster round ok\n");
    return 0;
}
