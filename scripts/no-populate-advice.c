/* Runs a command as a Linux kernel before 5.14 would run it in one respect: the two
 * populate advices of madvise(2), MADV_POPULATE_READ and MADV_POPULATE_WRITE, are answered
 * with EINVAL, which is what a kernel that does not have them answers. Cavalry's pre-touch
 * (crates/cavalry-shm/src/pretouch.rs; ADR 0027) then takes its touch loop, the path no
 * CI runner's kernel takes by itself. `just shm-exhaustion` runs its test under this,
 * so that a driver directory with no room is held to a refusal on such a kernel too
 * (work item 497; ADR 0175).
 *
 *     cc -O2 -o target/no-populate-advice scripts/no-populate-advice.c
 *     target/no-populate-advice <command> [arguments...]
 *
 * It installs a seccomp filter on itself and execs the command, which inherits it, as
 * does every process the command starts. No privilege is needed: the process first
 * gives up gaining any (PR_SET_NO_NEW_PRIVS). Every other system call, and madvise
 * with any other advice, is allowed as it was. Before the exec it asks for the advice
 * once itself and stops unless the answer is EINVAL, so that a run under it is never
 * a run without the filter. Linux only; it does not compile elsewhere, and the recipe
 * does not build it elsewhere.
 */
#include <errno.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>

/* The advices' numbers, which a C library older than the kernel's 5.14 does not name. */
#define POPULATE_READ 22
#define POPULATE_WRITE 23

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: no-populate-advice <command> [arguments...]\n");
        return 2;
    }
    /* The advice is madvise's third argument. Its low 32 bits are read, at the offset a
     * little-endian machine has them; every machine Cavalry is built for is one
     * (spec/README.md), and the check below stops the run on any other. */
    struct sock_filter filter[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_madvise, 0, 4),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[2])),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, POPULATE_READ, 1, 0),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, POPULATE_WRITE, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | EINVAL),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog program = {
        .len = (unsigned short)(sizeof filter / sizeof filter[0]),
        .filter = filter,
    };
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0) {
        fprintf(stderr, "no-populate-advice: PR_SET_NO_NEW_PRIVS: %s\n", strerror(errno));
        return 1;
    }
    if (prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program) != 0) {
        fprintf(stderr, "no-populate-advice: the seccomp filter was not installed: %s; "
                        "run it where a process may filter its own system calls\n",
                strerror(errno));
        return 1;
    }
    long page = sysconf(_SC_PAGESIZE);
    void *probe = mmap(NULL, (size_t)page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (probe == MAP_FAILED) {
        fprintf(stderr, "no-populate-advice: mmap: %s\n", strerror(errno));
        return 1;
    }
    for (int advice = POPULATE_READ; advice <= POPULATE_WRITE; advice++) {
        errno = 0;
        if (madvise(probe, (size_t)page, advice) != -1 || errno != EINVAL) {
            fprintf(stderr, "no-populate-advice: madvise with advice %d answered %s under the filter; "
                            "EINVAL expected, so the command was not run\n",
                    advice, errno == 0 ? "success" : strerror(errno));
            return 1;
        }
    }
    if (madvise(probe, (size_t)page, MADV_DONTNEED) != 0) {
        fprintf(stderr, "no-populate-advice: madvise with another advice failed under the filter: %s\n",
                strerror(errno));
        return 1;
    }
    munmap(probe, (size_t)page);
    execvp(argv[1], argv + 1);
    fprintf(stderr, "no-populate-advice: %s: %s\n", argv[1], strerror(errno));
    return 127;
}
