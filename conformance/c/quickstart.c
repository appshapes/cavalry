/* The first runner of conformance/ (CLAUDE.md, gate 1; ADR 0051): a C program over
 * crates/cavalry-ffi/include/cavalry.h that runs the ten-minute path's shape against a
 * driver, publish and subscribe over cavalry:ipc and cavalry:udp, record and replay,
 * every message once and in order, and checks what spec/c-abi.md says a reader
 * rejects. conformance/c/run.sh builds, links and runs it; just conformance runs that.
 * A failed check prints the function, the code and cavalry_errmsg and exits 1. */
/* clock_gettime and CLOCK_MONOTONIC are POSIX, not C11: glibc declares them under
 * -std=c11 only with this defined before the first include; macOS does as well. */
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
    cavalry_image_constants_t constants;
} receiver_t;

static int64_t frame_position(const cavalry_image_constants_t *c, int32_t term_id,
                              int32_t term_offset) {
    int32_t term_count = (int32_t)((uint32_t)term_id - (uint32_t)c->initial_term_id);
    return ((int64_t)term_count << c->position_bits_to_shift) + term_offset;
}

static void on_fragment(void *clientd, const uint8_t *buffer, size_t length,
                        const cavalry_header_t *header) {
    receiver_t *r = clientd;
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

/* Takes the image's constants once the poll has joined it, for the positions. */
static void take_constants(receiver_t *r) {
    if (r->have_constants) return;
    if (cavalry_subscription_image_count(r->subscription) < 1) return;
    cavalry_image_t *image = cavalry_subscription_image_at_index(r->subscription, 0);
    if (image == NULL) fail("cavalry_subscription_image_at_index");
    CHECK(cavalry_image_constants(image, &r->constants));
    r->have_constants = 1;
}

/* Every frame's position from the image's constants, once all are received. */
static void compute_positions(receiver_t *r) {
    if (!r->have_constants) {
        fprintf(stderr, "conformance/c: %s: no image constants after %" PRIu64 " messages\n",
                r->name, r->next);
        exit(1);
    }
    for (uint64_t i = 0; i < r->next; i++) {
        r->positions[i] = frame_position(&r->constants, r->term_ids[i], r->term_offsets[i]);
    }
}

/* Publishes MESSAGES messages by claim and commit on `publication`, polling `r` as it
 * goes, and drains `r` to the end. */
static void publish_and_receive(cavalry_publication_t *publication, receiver_t *r) {
    cavalry_buffer_claim_t claim;
    for (uint64_t i = 0; i < MESSAGES;) {
        /* The image joins on a poll; take its constants before the first frame is read. */
        if (cavalry_subscription_poll(r->subscription, on_fragment, r, 16) < 0)
            fail("cavalry_subscription_poll");
        take_constants(r);
        int64_t position = cavalry_publication_try_claim(publication, BODY, &claim);
        if (position == CAVALRY_PUBLICATION_ERROR) fail("cavalry_publication_try_claim");
        if (position < 0) continue; /* not connected or back pressured: poll and retry */
        memcpy(claim.data, &i, sizeof i);
        for (size_t b = sizeof i; b < BODY; b++) claim.data[b] = (uint8_t)(i + b);
        if (cavalry_buffer_claim_commit(&claim) != position) fail("cavalry_buffer_claim_commit");
        i++;
    }
    struct timespec until = deadline();
    while (r->next < MESSAGES) {
        if (cavalry_subscription_poll(r->subscription, on_fragment, r, 16) < 0)
            fail("cavalry_subscription_poll");
        take_constants(r);
        if (passed(&until)) {
            fprintf(stderr, "conformance/c: %s: received %" PRIu64 " of %d in %d s\n", r->name,
                    r->next, MESSAGES, WAIT_SECONDS);
            exit(1);
        }
    }
    compute_positions(r);
}

static int free_udp_port(void) {
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) { perror("socket"); exit(1); }
    struct sockaddr_in address = {0};
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
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
    cavalry_image_constants_t udp_image;
    take_constants(&udp);
    udp_image = udp.constants;
    printf("conformance/c: %d messages over %s by session %" PRId32
           " received once, in order, from %s\n", MESSAGES, udp_channel, udp.session_id,
           udp_image.source_identity);
    CHECK(cavalry_publication_close(udp_pub));
    CHECK(cavalry_subscription_close(udp_sub));

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
            fprintf(stderr, "conformance/c: replayed %" PRIu64 " of %d, ended %s\n", replayed.next,
                    MESSAGES, cavalry_replay_ended(replay, NULL, NULL) ? "yes" : "no");
            return 1;
        }
    }
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
