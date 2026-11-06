/* The first runner of conformance/ (CLAUDE.md, gate 1; ADR 0051): a C program over
 * crates/cavalry-ffi/include/cavalry.h that runs the ten-minute path's shape against a
 * driver, publish and subscribe over cavalry:ipc and cavalry:udp, record and replay,
 * every message once and in order, and checks what spec/c-abi.md says a reader
 * rejects. conformance/c/run.sh builds, links and runs it; just conformance runs that.
 * A failed check prints the function and the code, with cavalry_errmsg when the call
 * set the thread's error, and exits 1; every wait is bounded at WAIT_SECONDS. */
/* clock_gettime and CLOCK_MONOTONIC are POSIX, not C11: glibc declares them under
 * -std=c11 only with this defined before the first include; macOS does as well. The
 * loopback address is parsed with inet_pton, which POSIX declares, because macOS hides
 * INADDR_LOOPBACK, a BSD name, when _POSIX_C_SOURCE is defined. */
#define _POSIX_C_SOURCE 200809L
#include <arpa/inet.h>
#include <inttypes.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#include "cavalry.h"

#define MESSAGES 1000
#define BODY 64
#define STREAM 1001
#define REPLAY_STREAM 2001
#define WAIT_SECONDS 10

/* A wall-clock bound on each wait, in seconds from now. */
static struct timespec deadline(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    now.tv_sec += WAIT_SECONDS;
    return now;
}

static int passed(const struct timespec *at) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec > at->tv_sec || (now.tv_sec == at->tv_sec && now.tv_nsec > at->tv_nsec);
}

static void fail(const char *function) {
    fprintf(stderr, "conformance/c: %s failed with code %d: %s\n", function, cavalry_errcode(),
            cavalry_errmsg());
    exit(1);
}

#define CHECK(call) do { if ((call) != 0) fail(#call); } while (0)

/* What a subscription has received: the next index expected, and the frames'
 * positions computed from the image's constants, for the replay to compare. */
typedef struct {
    const char *name;
    uint64_t next;
    int32_t session_id;
    /* Each frame's term id and offset, kept from the handler; its position is computed
     * once the image's constants are held, since the poll that joins the image reads
     * its first frames before the caller can take them (spec/c-abi.md, "A frame's
     * position"). */
    int32_t term_ids[MESSAGES];
    int32_t term_offsets[MESSAGES];
    int64_t positions[MESSAGES];
    cavalry_subscription_t *subscription;
    int have_constants;
    /* The numeric fields of the frames' image's constants, which may be kept across
     * polls; its source identity is valid only until the next poll ("Lifetimes"), so
     * it is copied out, and the struct's pointer is null. */
    cavalry_image_constants_t constants;
    char source_identity[128];
} receiver_t;

static int64_t frame_position(const cavalry_image_constants_t *c, int32_t term_id,
                              int32_t term_offset) {
    int32_t term_count = (int32_t)((uint32_t)term_id - (uint32_t)c->initial_term_id);
    return ((int64_t)term_count << c->position_bits_to_shift) + term_offset;
}

static void on_fragment(void *clientd, const uint8_t *buffer, size_t length,
                        const cavalry_header_t *header) {
    receiver_t *r = clientd;
    /* Every message has arrived, so this fragment is one of them again, and the arrays
     * hold no room for it. */
    if (r->next >= MESSAGES) {
        fprintf(stderr, "conformance/c: %s: a fragment after all %d messages (once and in "
                "order)\n", r->name, MESSAGES);
        exit(1);
    }
    cavalry_header_values_t values;
    CHECK(cavalry_header_values(header, &values));
    if (length != BODY) {
        fprintf(stderr, "conformance/c: %s: fragment %" PRIu64 " is %zu bytes, not %d\n",
                r->name, r->next, length, BODY);
        exit(1);
    }
    uint64_t index;
    memcpy(&index, buffer, sizeof index);
    if (index != r->next) {
        fprintf(stderr, "conformance/c: %s: expected message %" PRIu64 ", got %" PRIu64
                " (once and in order)\n", r->name, r->next, index);
        exit(1);
    }
    for (size_t i = sizeof index; i < length; i++) {
        if (buffer[i] != (uint8_t)(index + i)) {
            fprintf(stderr, "conformance/c: %s: message %" PRIu64 " byte %zu is wrong\n",
                    r->name, index, i);
            exit(1);
        }
    }
    /* spec/frame.md: a data frame, unfragmented, of the stream, from one session. */
    if (values.frame.type != CAVALRY_FRAME_TYPE_DATA ||
        values.frame.flags != CAVALRY_FLAGS_UNFRAGMENTED || values.frame.version != 0 ||
        values.frame.frame_length != 32 + BODY || values.frame.term_offset % 32 != 0) {
        fprintf(stderr, "conformance/c: %s: message %" PRIu64 " has a header the page forbids\n",
                r->name, index);
        exit(1);
    }
    if (r->next == 0) {
        r->session_id = values.frame.session_id;
    } else if (values.frame.session_id != r->session_id) {
        fprintf(stderr, "conformance/c: %s: the session id changed mid-stream\n", r->name);
        exit(1);
    }
    r->term_ids[index] = values.frame.term_id;
    r->term_offsets[index] = values.frame.term_offset;
    r->next++;
}

/* Takes the constants of the image the frames came from, for the positions, once a poll
 * has delivered a frame: the image is the one whose session id the frames' header
 * carries (spec/c-abi.md, "A frame's position"). The source identity is copied now,
 * while the image pointer is valid, and the struct keeps no pointer to it. */
static void take_constants(receiver_t *r) {
    if (r->have_constants || r->next == 0) return;
    int count = cavalry_subscription_image_count(r->subscription);
    if (count < 0) fail("cavalry_subscription_image_count");
    for (int i = 0; i < count; i++) {
        cavalry_image_t *image = cavalry_subscription_image_at_index(r->subscription, (size_t)i);
        if (image == NULL) fail("cavalry_subscription_image_at_index");
        cavalry_image_constants_t constants;
        CHECK(cavalry_image_constants(image, &constants));
        if (constants.session_id != r->session_id) continue;
        int written = snprintf(r->source_identity, sizeof r->source_identity, "%s",
                               constants.source_identity);
        if (written < 0 || (size_t)written >= sizeof r->source_identity) {
            fprintf(stderr, "conformance/c: %s: the image's source identity is longer than %zu "
                    "bytes\n", r->name, sizeof r->source_identity - 1);
            exit(1);
        }
        constants.source_identity = NULL;
        r->constants = constants;
        r->have_constants = 1;
        return;
    }
}

/* Every frame's position from the image's constants, once all are received. */
static void compute_positions(receiver_t *r) {
    if (!r->have_constants) {
        fprintf(stderr, "conformance/c: %s: no image of session %" PRId32 " after %" PRIu64
                " messages\n", r->name, r->session_id, r->next);
        exit(1);
    }
    for (uint64_t i = 0; i < r->next; i++) {
        r->positions[i] = frame_position(&r->constants, r->term_ids[i], r->term_offsets[i]);
    }
}

/* After the last message, one more poll delivers nothing. A drain stops at the last
 * message, so without it a fragment delivered again after that one would go unseen. */
static void expect_no_more(receiver_t *r) {
    int count = cavalry_subscription_poll(r->subscription, on_fragment, r, 16);
    if (count < 0) fail("cavalry_subscription_poll");
    if (count != 0) {
        fprintf(stderr, "conformance/c: %s: a poll after all %d messages delivered %d more\n",
                r->name, MESSAGES, count);
        exit(1);
    }
}

/* A result code's name (spec/c-abi.md, "Result codes"). */
static const char *result_name(int64_t code) {
    switch (code) {
    case CAVALRY_PUBLICATION_NOT_CONNECTED: return "CAVALRY_PUBLICATION_NOT_CONNECTED";
    case CAVALRY_PUBLICATION_BACK_PRESSURED: return "CAVALRY_PUBLICATION_BACK_PRESSURED";
    case CAVALRY_PUBLICATION_ADMIN_ACTION: return "CAVALRY_PUBLICATION_ADMIN_ACTION";
    case CAVALRY_PUBLICATION_CLOSED: return "CAVALRY_PUBLICATION_CLOSED";
    case CAVALRY_PUBLICATION_MAX_POSITION_EXCEEDED: return "CAVALRY_PUBLICATION_MAX_POSITION_EXCEEDED";
    case CAVALRY_PUBLICATION_ERROR: return "CAVALRY_PUBLICATION_ERROR";
    default: return "not a result code";
    }
}

/* Publishes MESSAGES messages by claim and commit on `publication`, polling `r` as it
 * goes, drains `r` to the end and polls it once more for nothing. */
/* Publishes MESSAGES on the publication while polling every receiver, then polls until
 * each has every message: one receiver for a stream to one endpoint, two for a control
 * channel's stream to two destinations (ADR 0053). */
static void publish_and_receive_all(cavalry_publication_t *publication, receiver_t **rs, int n) {
    cavalry_buffer_claim_t claim;
    struct timespec publish_until = deadline();
    receiver_t *r = rs[0];
    for (uint64_t i = 0; i < MESSAGES;) {
        /* The image joins on a poll; take its constants once a frame from it is read. */
        for (int k = 0; k < n; k++) {
            if (cavalry_subscription_poll(rs[k]->subscription, on_fragment, rs[k], 16) < 0)
                fail("cavalry_subscription_poll");
            take_constants(rs[k]);
        }
        int64_t position = cavalry_publication_try_claim(publication, BODY, &claim);
        if (position == CAVALRY_PUBLICATION_ERROR) fail("cavalry_publication_try_claim");
        /* CLOSED and MAX_POSITION_EXCEEDED never clear, and a result code sets no error,
         * so the code itself is the message. */
        if (position == CAVALRY_PUBLICATION_CLOSED ||
            position == CAVALRY_PUBLICATION_MAX_POSITION_EXCEEDED) {
            fprintf(stderr, "conformance/c: %s: cavalry_publication_try_claim returned %s (%" PRId64
                    ") after %" PRIu64 " of %d messages\n", r->name, result_name(position),
                    position, i, MESSAGES);
            exit(1);
        }
        if (position < 0) {
            /* Not connected, back pressured or an admin action: poll and retry, within
             * the bound. */
            if (passed(&publish_until)) {
                fprintf(stderr, "conformance/c: %s: published %" PRIu64 " of %d in %d s; the last "
                        "claim returned %s (%" PRId64 ")\n", r->name, i, MESSAGES, WAIT_SECONDS,
                        result_name(position), position);
                exit(1);
            }
            continue;
        }
        memcpy(claim.data, &i, sizeof i);
        for (size_t b = sizeof i; b < BODY; b++) claim.data[b] = (uint8_t)(i + b);
        if (cavalry_buffer_claim_commit(&claim) != position) fail("cavalry_buffer_claim_commit");
        i++;
    }
    struct timespec until = deadline();
    for (int k = 0; k < n; k++) {
        while (rs[k]->next < MESSAGES) {
            if (cavalry_subscription_poll(rs[k]->subscription, on_fragment, rs[k], 16) < 0)
                fail("cavalry_subscription_poll");
            take_constants(rs[k]);
            if (passed(&until)) {
                fprintf(stderr, "conformance/c: %s: received %" PRIu64 " of %d in %d s\n",
                        rs[k]->name, rs[k]->next, MESSAGES, WAIT_SECONDS);
                exit(1);
            }
        }
    }
    expect_no_more(r);
    compute_positions(r);
}

static void publish_and_receive(cavalry_publication_t *publication, receiver_t *r) {
    receiver_t *rs[1] = {r};
    publish_and_receive_all(publication, rs, 1);
}

static int free_udp_port(void) {
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) { perror("socket"); exit(1); }
    struct sockaddr_in address = {0};
    address.sin_family = AF_INET;
    /* The address the channel string names. */
    if (inet_pton(AF_INET, "127.0.0.1", &address.sin_addr) != 1) {
        fprintf(stderr, "conformance/c: inet_pton did not parse 127.0.0.1\n");
        exit(1);
    }
    socklen_t length = sizeof address;
    if (bind(fd, (struct sockaddr *)&address, length) < 0 ||
        getsockname(fd, (struct sockaddr *)&address, &length) < 0) {
        perror("bind");
        exit(1);
    }
    int port = ntohs(address.sin_port);
    close(fd);
    return port;
}

int main(void) {
    /* A line at a time, so that a failure on stderr follows the lines before it in a
     * CI log, where stdout is a pipe. */
    setvbuf(stdout, NULL, _IOLBF, 0);
    /* What a reader rejects, both ways: another version is refused with the code, and
     * the header's own version is not. */
    cavalry_context_t *context = NULL;
    if (cavalry_context_init_v(&context, CAVALRY_ABI_VERSION + 1) != -1 ||
        cavalry_errcode() != CAVALRY_ERR_ABI_VERSION || context != NULL) {
        fprintf(stderr, "conformance/c: a mismatched ABI version was not refused\n");
        return 1;
    }
    if (cavalry_abi_version() != CAVALRY_ABI_VERSION) {
        fprintf(stderr, "conformance/c: the library's ABI version is %d, the header's %d\n",
                cavalry_abi_version(), CAVALRY_ABI_VERSION);
        return 1;
    }
    CHECK(cavalry_context_init(&context));
    /* The driver directory from the environment, as run.sh set it. */
    cavalry_t *client = NULL;
    CHECK(cavalry_init(&client, context));
    printf("conformance/c: client %" PRId64 " on %s, library %s, ABI version %d\n",
           cavalry_client_id(client), cavalry_dir(client), cavalry_version_full(),
           cavalry_abi_version());

    /* 1. Record the stream, then publish 1,000 messages over cavalry:ipc and read them
     * back, once and in order. */
    cavalry_recording_t *recording = NULL;
    cavalry_subscription_t *ipc_sub = NULL;
    cavalry_publication_t *ipc_pub = NULL;
    CHECK(cavalry_add_subscription(&ipc_sub, client, "cavalry:ipc", STREAM));
    CHECK(cavalry_add_publication(&ipc_pub, client, "cavalry:ipc", STREAM));
    CHECK(cavalry_start_recording(&recording, client, "cavalry:ipc", STREAM));
    cavalry_recording_constants_t rec;
    CHECK(cavalry_recording_constants(recording, &rec));
    static receiver_t ipc = {.name = "cavalry:ipc"};
    ipc.subscription = ipc_sub;
    publish_and_receive(ipc_pub, &ipc);
    int64_t end = cavalry_publication_position(ipc_pub);
    if (end < 0) fail("cavalry_publication_position");
    printf("conformance/c: %d messages over cavalry:ipc by session %" PRId32
           " received once, in order; the stream ends at %" PRId64 "\n", MESSAGES,
           ipc.session_id, end);
    struct timespec sync_until = deadline();
    while (cavalry_recording_synced_position(recording) < end) {
        if (passed(&sync_until)) {
            fprintf(stderr, "conformance/c: the recording synced %" PRId64 " of %" PRId64 " in %d s\n",
                    cavalry_recording_synced_position(recording), end, WAIT_SECONDS);
            return 1;
        }
    }
    CHECK(cavalry_stop_recording(client, rec.recording_id));
    CHECK(cavalry_recording_close(recording));
    CHECK(cavalry_publication_close(ipc_pub));

    /* 2. The same 1,000 over cavalry:udp on the loopback, through the driver's sender
     * and receiver. */
    char udp_channel[64];
    snprintf(udp_channel, sizeof udp_channel, "cavalry:udp?endpoint=127.0.0.1:%d", free_udp_port());
    cavalry_subscription_t *udp_sub = NULL;
    cavalry_publication_t *udp_pub = NULL;
    CHECK(cavalry_add_subscription(&udp_sub, client, udp_channel, STREAM));
    CHECK(cavalry_add_publication(&udp_pub, client, udp_channel, STREAM));
    static receiver_t udp = {.name = "cavalry:udp"};
    udp.subscription = udp_sub;
    publish_and_receive(udp_pub, &udp);
    /* The image's source identity as take_constants copied it, inside its lifetime. */
    printf("conformance/c: %d messages over %s by session %" PRId32
           " received once, in order, from %s\n", MESSAGES, udp_channel, udp.session_id,
           udp.source_identity);
    CHECK(cavalry_publication_close(udp_pub));
    CHECK(cavalry_subscription_close(udp_sub));

    /* 2b. The same 1,000 over a control channel to two destinations, each a subscription
     * on a loopback port of its own, under fc=min: multi-destination cast (ADR 0053),
     * through cavalry_add_destination and cavalry_remove_destination (ADR 0056). */
    char control_channel[96], destination_a[64], destination_b[64];
    snprintf(control_channel, sizeof control_channel,
             "cavalry:udp?control=127.0.0.1:%d|control-mode=manual|fc=min", free_udp_port());
    snprintf(destination_a, sizeof destination_a, "cavalry:udp?endpoint=127.0.0.1:%d", free_udp_port());
    snprintf(destination_b, sizeof destination_b, "cavalry:udp?endpoint=127.0.0.1:%d", free_udp_port());
    cavalry_subscription_t *sub_a = NULL, *sub_b = NULL;
    cavalry_publication_t *mdc_pub = NULL;
    CHECK(cavalry_add_subscription(&sub_a, client, destination_a, STREAM));
    CHECK(cavalry_add_subscription(&sub_b, client, destination_b, STREAM));
    CHECK(cavalry_add_publication(&mdc_pub, client, control_channel, STREAM));
    CHECK(cavalry_add_destination(client, mdc_pub, destination_a));
    CHECK(cavalry_add_destination(client, mdc_pub, destination_b));
    /* A duplicate add and a remove of one never added are success and change nothing;
     * a destination the control address cannot send to is refused when added, code 1
     * (spec/client-protocol.md, "Commands"). */
    CHECK(cavalry_add_destination(client, mdc_pub, destination_a));
    CHECK(cavalry_remove_destination(client, mdc_pub, "cavalry:udp?endpoint=127.0.0.1:1"));
    if (cavalry_add_destination(client, mdc_pub, "cavalry:udp?endpoint=[::1]:40123") != -1 ||
        cavalry_errcode() != 1) {
        fprintf(stderr, "conformance/c: a destination of the other family was not refused with code 1: %s\n",
                cavalry_errmsg());
        return 1;
    }
    static receiver_t mdc_a = {.name = "destination a"}, mdc_b = {.name = "destination b"};
    mdc_a.subscription = sub_a;
    mdc_b.subscription = sub_b;
    receiver_t *both[2] = {&mdc_a, &mdc_b};
    publish_and_receive_all(mdc_pub, both, 2);
    printf("conformance/c: %d messages over %s to two destinations by session %" PRId32
           " received once, in order, at each\n", MESSAGES, control_channel, mdc_a.session_id);
    CHECK(cavalry_remove_destination(client, mdc_pub, destination_b));
    CHECK(cavalry_publication_close(mdc_pub));
    CHECK(cavalry_subscription_close(sub_a));
    CHECK(cavalry_subscription_close(sub_b));

    /* 3. Replay the recording onto stream 2001: the same checks, each frame at the
     * position the recording holds it at (ADR 0030), and the end reported. */
    cavalry_subscription_t *replay_sub = NULL;
    CHECK(cavalry_add_subscription(&replay_sub, client, "cavalry:ipc", REPLAY_STREAM));
    cavalry_replay_t *replay = NULL;
    CHECK(cavalry_start_replay(&replay, client, rec.recording_id, CAVALRY_NULL_POSITION,
                               CAVALRY_NULL_LENGTH, REPLAY_STREAM, "cavalry:ipc"));
    static receiver_t replayed = {.name = "replay"};
    replayed.subscription = replay_sub;
    int64_t ended_at = CAVALRY_NULL_POSITION;
    int32_t reason = -1;
    struct timespec replay_until = deadline();
    for (;;) {
        if (cavalry_subscription_poll(replay_sub, on_fragment, &replayed, 16) < 0)
            fail("cavalry_subscription_poll");
        take_constants(&replayed);
        if (replayed.next >= MESSAGES && cavalry_replay_ended(replay, &ended_at, &reason)) break;
        if (passed(&replay_until)) {
            /* cavalry_replay_ended refuses a null pointer, so both are passed. */
            if (cavalry_replay_ended(replay, &ended_at, &reason)) {
                fprintf(stderr, "conformance/c: replayed %" PRIu64 " of %d in %d s; the replay "
                        "ended at %" PRId64 " with reason %" PRId32 "\n", replayed.next,
                        MESSAGES, WAIT_SECONDS, ended_at, reason);
            } else {
                fprintf(stderr, "conformance/c: replayed %" PRIu64 " of %d in %d s; the replay "
                        "has not ended, at position %" PRId64 "\n", replayed.next, MESSAGES,
                        WAIT_SECONDS, cavalry_replay_position(replay));
            }
            return 1;
        }
    }
    expect_no_more(&replayed);
    compute_positions(&replayed);
    if (ended_at != end || reason != CAVALRY_REPLAY_STOP_ENDED) {
        fprintf(stderr, "conformance/c: the replay ended at %" PRId64 " with reason %d, not at %"
                PRId64 " with %d\n", ended_at, reason, end, CAVALRY_REPLAY_STOP_ENDED);
        return 1;
    }
    for (int i = 0; i < MESSAGES; i++) {
        if (ipc.positions[i] == CAVALRY_NULL_POSITION || replayed.positions[i] != ipc.positions[i]) {
            fprintf(stderr, "conformance/c: message %d replayed at %" PRId64 ", recorded at %" PRId64 "\n",
                    i, replayed.positions[i], ipc.positions[i]);
            return 1;
        }
    }
    printf("conformance/c: %d messages replayed on stream %d at the positions the recording "
           "holds them, ended at %" PRId64 "\n", MESSAGES, REPLAY_STREAM, ended_at);
    CHECK(cavalry_replay_close(replay));
    CHECK(cavalry_subscription_close(replay_sub));
    CHECK(cavalry_subscription_close(ipc_sub));
    CHECK(cavalry_close(client));
    CHECK(cavalry_context_close(context));
    printf("conformance/c: ok\n");
    return 0;
}
