/* The Linux half of the K6 paired benchmarks, and the guest's init.
 *
 * This runs as PID 1 of a Linux guest booted on the same virtual machine as
 * the native runs -- the same QEMU, machine, processor model, processor count,
 * memory and firmware -- from an initramfs that holds this program, the plan,
 * Thalyx's engine and the model. It runs the same bench.c over the closest
 * primitives Linux has and reports on the serial port, one line per note, in a
 * form the host parses beside the native records:
 *
 *   K6N <code> <value>     a note, both hexadecimal, the codes of k6.h
 *   K6T <key> <text>       what this guest is: kernel release, command line,
 *                          the kernel's own view of CPU vulnerabilities
 *
 * What each primitive is, is decided in abi/schema/k6-bench-v1.json, not
 * here: a SOCK_SEQPACKET pair with SO_PASSCRED for a call, SCM_RIGHTS for
 * capabilities, mmap for mapped memory, a sealed memfd for a sealed object,
 * F_DUPFD_CLOEXEC for a derived capability, a futex for a wake, cgroup v2
 * cpu.max for a budget, SIGKILL and waitpid for closure, and Thalyx's own
 * engine, unchanged, over its own frames for the real load.
 */

#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/futex.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/io.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include "bench.h"

#define PAIRS 4
#define ENGINE "/thalyx-engine"
#define MODEL "/tiny.gguf"
#define PROMPT_FILE "/tmp/k6-prompt.txt"
#define GRAMMAR_FILE "/tmp/k6-grammar.gbnf"

static int out_fd = 2;

/* Reports go to QEMU's debug console port, one `outsb` per line, and not to
 * the serial console. The serial console is a tty: the kernel paces it at the
 * baud rate the line is set to, and the first guest spent two minutes of a
 * two-minute run writing 118 KB of samples at 9600 baud. The port is used only
 * between measured operations, never inside one. */
#define DEBUGCON_PORT 0xe9
static int debugcon;

static void report(const char *line, size_t n)
{
    if (debugcon) {
        __asm__ __volatile__("rep outsb" : "+S"(line), "+c"(n) : "d"((uint16_t)DEBUGCON_PORT) : "memory");
    } else {
        (void)!write(out_fd, line, n);
    }
}

/* ------------------------------------------------------ backend basics */

uint64_t plat_now_ns(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

void plat_emit(uint64_t code, uint64_t value)
{
    char line[96];
    int n;
    if (code == K6_NOTE_BEGIN || code == K6_NOTE_END) {
        /* When, by this guest's clock: the native records carry the kernel's
         * time, and a plan that took longer on one side than the other should
         * say where. */
        n = snprintf(line, sizeof(line), "K6T at %llu\n", (unsigned long long)plat_now_ns());
        if (n > 0) { report(line, (size_t)n); }
    }
    n = snprintf(line, sizeof(line), "K6N %llx %llx\n", (unsigned long long)code,
                 (unsigned long long)value);
    if (n > 0) { report(line, (size_t)n); }
}

static void text(const char *key, const char *value)
{
    char line[512];
    int n = snprintf(line, sizeof(line), "K6T %s %s\n", key, value);
    if (n > 0) { report(line, (size_t)(n < (int)sizeof(line) ? n : (int)sizeof(line) - 1)); }
}

/* Helper threads: three, as the native client is built with. */
typedef struct {
    bench_fn body;
    void *argument;
} Helper;

static pthread_t helpers[4];
static Helper helper_args[4];

static void *helper_main(void *argument)
{
    Helper *helper = argument;
    helper->body(helper->argument);
    return NULL;
}

unsigned plat_helper_threads(void) { return 3; }

int plat_thread_start(unsigned index, bench_fn body, void *argument)
{
    if (index == 0 || index > 3) { return -1; }
    helper_args[index].body = body;
    helper_args[index].argument = argument;
    return pthread_create(&helpers[index], NULL, helper_main, &helper_args[index]) == 0 ? 0 : -1;
}

int plat_thread_join(unsigned index)
{
    if (index == 0 || index > 3) { return -1; }
    return pthread_join(helpers[index], NULL) == 0 ? 0 : -1;
}

static uint32_t wake_word;

static long futex(uint32_t *word, int op, uint32_t value)
{
    return syscall(SYS_futex, word, op, value, NULL, NULL, 0);
}

void plat_block(void)
{
    while (__atomic_exchange_n(&wake_word, 0u, __ATOMIC_ACQUIRE) == 0) {
        futex(&wake_word, FUTEX_WAIT_PRIVATE, 0);
    }
}

void plat_wake(void)
{
    __atomic_store_n(&wake_word, 1u, __ATOMIC_RELEASE);
    futex(&wake_word, FUTEX_WAKE_PRIVATE, 1);
}

/* ---------------------------------------------------------------- IPC */

static int client_sock[PAIRS];
static pid_t server_pid = -1;

typedef union {
    struct cmsghdr align;
    char space[CMSG_SPACE(sizeof(struct ucred)) + CMSG_SPACE(4 * sizeof(int))];
} Control;

/* One answerer thread per pair, in one server process: the shape of the
 * native server domain, which has one thread per endpoint. Every message
 * arrives with the sender's credentials, because SO_PASSCRED is set; the
 * descriptors a message carried are closed; the payload is sent back. */
static void *serve_pair(void *argument)
{
    int sock = (int)(intptr_t)argument;
    char buffer[512];
    for (;;) {
        Control control;
        struct iovec iov = {buffer, sizeof(buffer)};
        struct msghdr msg;
        memset(&msg, 0, sizeof(msg));
        msg.msg_iov = &iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.space;
        msg.msg_controllen = sizeof(control.space);
        ssize_t got = recvmsg(sock, &msg, 0);
        if (got < 0) {
            if (errno == EINTR) { continue; }
            return NULL;
        }
        for (struct cmsghdr *c = CMSG_FIRSTHDR(&msg); c != NULL; c = CMSG_NXTHDR(&msg, c)) {
            if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS) {
                int count = (int)((c->cmsg_len - CMSG_LEN(0)) / sizeof(int));
                int fds[4];
                memcpy(fds, CMSG_DATA(c), (size_t)count * sizeof(int));
                for (int i = 0; i < count && i < 4; i++) { close(fds[i]); }
            }
        }
        if (send(sock, buffer, (size_t)got, MSG_NOSIGNAL) < 0) { return NULL; }
    }
}

static void start_server(void)
{
    int server_sock[PAIRS];
    for (int p = 0; p < PAIRS; p++) {
        int sv[2];
        if (socketpair(AF_UNIX, SOCK_SEQPACKET, 0, sv) != 0) { text("error", "socketpair"); return; }
        client_sock[p] = sv[0];
        server_sock[p] = sv[1];
    }
    server_pid = fork();
    if (server_pid == 0) {
        for (int p = 0; p < PAIRS; p++) {
            close(client_sock[p]);
            int one = 1;
            setsockopt(server_sock[p], SOL_SOCKET, SO_PASSCRED, &one, sizeof(one));
        }
        pthread_t threads[PAIRS];
        for (int p = 1; p < PAIRS; p++) {
            pthread_create(&threads[p], NULL, serve_pair, (void *)(intptr_t)server_sock[p]);
        }
        serve_pair((void *)(intptr_t)server_sock[0]);
        _exit(0);
    }
    for (int p = 0; p < PAIRS; p++) { close(server_sock[p]); }
}

static char request_bytes[256];

static int64_t call_raw(unsigned pair, uint32_t payload, const int *fds, uint32_t count)
{
    char answer[512];
    Control control;
    struct iovec iov = {request_bytes, payload};
    struct msghdr msg;
    memset(&msg, 0, sizeof(msg));
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    if (count) {
        msg.msg_control = control.space;
        msg.msg_controllen = CMSG_SPACE(count * sizeof(int));
        struct cmsghdr *c = CMSG_FIRSTHDR(&msg);
        c->cmsg_level = SOL_SOCKET;
        c->cmsg_type = SCM_RIGHTS;
        c->cmsg_len = CMSG_LEN(count * sizeof(int));
        memcpy(CMSG_DATA(c), fds, count * sizeof(int));
    }
    if (sendmsg(client_sock[pair], &msg, MSG_NOSIGNAL) < 0) { return -errno; }
    ssize_t got = recv(client_sock[pair], answer, sizeof(answer), 0);
    if (got < 0) { return -errno; }
    return (uint32_t)got == payload ? 0 : -EPROTO;
}

int64_t plat_ipc_pair_call(unsigned pair) { return call_raw(pair, 0, NULL, 0); }

/* -------------------------------------------------------------- entries */

static int cap_fds[4];
static uint32_t cap_count;
static int derive_fd = -1;

int64_t plat_prepare(uint32_t bench, uint32_t param)
{
    switch (bench) {
    case K6_BENCH_IPC_CALL:
        for (uint32_t i = 0; i < sizeof(request_bytes); i++) { request_bytes[i] = (char)i; }
        return param <= 256 ? 0 : -EINVAL;
    case K6_BENCH_IPC_CAPS:
        if (param == 0 || param > 4) { return -EINVAL; }
        cap_count = 0;
        for (uint32_t i = 0; i < param; i++) {
            int fd = memfd_create("k6cap", MFD_CLOEXEC);
            if (fd < 0 || ftruncate(fd, 4096) != 0) { plat_release(bench, param); return -errno; }
            cap_fds[cap_count++] = fd;
        }
        return 0;
    case K6_BENCH_CAP_DERIVE:
        derive_fd = memfd_create("k6derive", MFD_CLOEXEC);
        if (derive_fd < 0 || ftruncate(derive_fd, 4096) != 0) { return -errno; }
        return 0;
    case K6_BENCH_IPC_LINEAGE:
    case K6_BENCH_AUDIT_DRAIN:
        /* Native-only in the schema: nothing on this side answers the
         * question, and nothing is measured in its place. */
        return -ENOSYS;
    default:
        return 0;
    }
}

int64_t plat_op(uint32_t bench, uint32_t param)
{
    switch (bench) {
    case K6_BENCH_ENTRY_VERSION:
        return syscall(SYS_getppid) >= 0 ? 0 : -errno;
    case K6_BENCH_ENTRY_CLOCK: {
        struct timespec ts;
        return syscall(SYS_clock_gettime, CLOCK_MONOTONIC, &ts) == 0 ? 0 : -errno;
    }
    case K6_BENCH_IPC_CALL:
        return call_raw(0, param, NULL, 0);
    case K6_BENCH_IPC_CAPS:
        return call_raw(0, 0, cap_fds, cap_count);
    case K6_BENCH_MEM_MAP: {
        size_t bytes = (size_t)param * 4096u;
        volatile uint8_t *p = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p == MAP_FAILED) { return -errno; }
        for (uint32_t page = 0; page < param; page++) { p[(size_t)page * 4096u] = (uint8_t)page; }
        return munmap((void *)p, bytes) == 0 ? 0 : -errno;
    }
    case K6_BENCH_MEM_SEAL: {
        size_t bytes = (size_t)param * 4096u;
        int fd = memfd_create("k6seal", MFD_CLOEXEC | MFD_ALLOW_SEALING);
        if (fd < 0) { return -errno; }
        int64_t status = 0;
        if (ftruncate(fd, (off_t)bytes) != 0) { status = -errno; }
        if (status == 0) {
            volatile uint8_t *w = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
            if (w == MAP_FAILED) {
                status = -errno;
            } else {
                for (uint32_t page = 0; page < param; page++) { w[(size_t)page * 4096u] = (uint8_t)(page + 1); }
                /* Linux refuses F_SEAL_WRITE while a writable mapping exists,
                 * so the program removes it first; the native seal withdraws
                 * it itself. */
                munmap((void *)w, bytes);
            }
        }
        if (status == 0 && fcntl(fd, F_ADD_SEALS, F_SEAL_WRITE | F_SEAL_SHRINK | F_SEAL_GROW) != 0) {
            status = -errno;
        }
        if (status == 0) {
            volatile uint8_t *r = mmap(NULL, bytes, PROT_READ, MAP_SHARED, fd, 0);
            if (r == MAP_FAILED) {
                status = -errno;
            } else {
                if (r[0] != 1) { status = -EPROTO; }
                munmap((void *)r, bytes);
            }
        }
        close(fd);
        return status;
    }
    case K6_BENCH_CAP_DERIVE: {
        int fd = fcntl(derive_fd, F_DUPFD_CLOEXEC, 0);
        if (fd < 0) { return -errno; }
        return close(fd) == 0 ? 0 : -errno;
    }
    default:
        return -ENOSYS;
    }
}

void plat_release(uint32_t bench, uint32_t param)
{
    (void)param;
    if (bench == K6_BENCH_IPC_CAPS) {
        for (uint32_t i = 0; i < cap_count; i++) { close(cap_fds[i]); }
        cap_count = 0;
    }
    if (bench == K6_BENCH_CAP_DERIVE && derive_fd >= 0) {
        close(derive_fd);
        derive_fd = -1;
    }
}

/* ----------------------------------------------------------- quota.share */

static int write_file(const char *path, const char *value)
{
    int fd = open(path, O_WRONLY | O_CLOEXEC);
    if (fd < 0) { return -errno; }
    ssize_t n = write(fd, value, strlen(value));
    int saved = errno;
    close(fd);
    return n < 0 ? -saved : 0;
}

static uint32_t child_samples[1024];

/* A child in a cgroup whose cpu.max is the budget, spinning as the native
 * quota domain does and reporting its own samples. */
static int64_t quota_share(uint32_t percent)
{
    char limit[64];
    snprintf(limit, sizeof(limit), "%u %llu", (unsigned)(percent * (K6_SLICE_NS / 1000u) / 100u),
             (unsigned long long)(K6_SLICE_NS / 1000u));
    mkdir("/sys/fs/cgroup/k6q", 0755);
    int64_t status = write_file("/sys/fs/cgroup/cgroup.subtree_control", "+cpu");
    if (status == 0) { status = write_file("/sys/fs/cgroup/k6q/cpu.max", limit); }
    if (status != 0) { return status; }
    pid_t child = fork();
    if (child == 0) {
        if (write_file("/sys/fs/cgroup/k6q/cgroup.procs", "0") != 0) { _exit(2); }
        bench_calibrate();
        int64_t n = bench_spin_share(1000000000ull, child_samples, 1024);
        if (n > 0) { bench_emit_samples(child_samples, (uint32_t)n); }
        _exit(0);
    }
    if (child < 0) { return -errno; }
    int st = 0;
    waitpid(child, &st, 0);
    return WIFEXITED(st) && WEXITSTATUS(st) == 0 ? 0 : -EPROTO;
}

/* ---------------------------------------------------------- closure.unit */

static int64_t closure_unit(uint32_t *out, uint32_t n)
{
    for (uint32_t i = 0; i < n; i++) {
        int pipefd[2];
        if (pipe(pipefd) != 0) { return -errno; }
        pid_t child = fork();
        if (child == 0) {
            char byte;
            close(pipefd[1]);
            (void)!read(pipefd[0], &byte, 1);
            _exit(0);
        }
        close(pipefd[0]);
        /* Give the child time to be blocked in its read, as the native side
         * waits for its idle domain to be running before closing it. */
        struct timespec wait = {0, 2000000};
        nanosleep(&wait, NULL);
        uint64_t c0 = bench_cycles();
        kill(child, SIGKILL);
        int st = 0;
        waitpid(child, &st, 0);
        uint64_t c1 = bench_cycles();
        close(pipefd[1]);
        uint64_t d = c1 - c0;
        out[i] = d >= K6_LOST_SAMPLE ? (uint32_t)(K6_LOST_SAMPLE - 1) : (uint32_t)d;
    }
    return (int64_t)n;
}

/* ---------------------------------------------------------------- engine */

typedef struct {
    pid_t pid;
    int to;
    int from;
} Engine;

static Engine resident = {-1, -1, -1};

static int read_exactly(int fd, void *into, size_t n)
{
    uint8_t *p = into;
    while (n) {
        ssize_t got = read(fd, p, n);
        if (got <= 0) {
            if (got < 0 && errno == EINTR) { continue; }
            return -1;
        }
        p += got;
        n -= (size_t)got;
    }
    return 0;
}

static int write_all(int fd, const void *from, size_t n)
{
    const uint8_t *p = from;
    while (n) {
        ssize_t put = write(fd, p, n);
        if (put <= 0) {
            if (put < 0 && errno == EINTR) { continue; }
            return -1;
        }
        p += put;
        n -= (size_t)put;
    }
    return 0;
}

/* Starts Thalyx's engine as Thalyx does on Linux and waits for its ready
 * frame. What it prints besides frames goes nowhere, as on the native side. */
static int engine_start(Engine *engine)
{
    int to[2], from[2];
    if (pipe(to) != 0 || pipe(from) != 0) { return -errno; }
    pid_t pid = fork();
    if (pid == 0) {
        dup2(to[0], 0);
        dup2(from[1], 1);
        int null = open("/dev/null", O_WRONLY);
        if (null >= 0) { dup2(null, 2); }
        close(to[1]);
        close(from[0]);
        char ctx[16], threads[16];
        snprintf(ctx, sizeof(ctx), "%u", (unsigned)K6_ENGINE_CONTEXT_TOKENS);
        snprintf(threads, sizeof(threads), "%u", (unsigned)K6_ENGINE_COMPUTE_THREADS);
        char *argv[] = {ENGINE, "-m", MODEL, "--ctx", ctx, "--threads", threads, NULL};
        execv(ENGINE, argv);
        _exit(127);
    }
    close(to[0]);
    close(from[1]);
    if (pid < 0) { return -errno; }
    engine->pid = pid;
    engine->to = to[1];
    engine->from = from[0];
    uint8_t ready[24];
    if (read_exactly(engine->from, ready, sizeof(ready)) != 0 || memcmp(ready, "THR1", 4) != 0) {
        return -EPROTO;
    }
    return 0;
}

static void engine_stop(Engine *engine, int signal)
{
    if (engine->pid <= 0) { return; }
    if (signal) {
        kill(engine->pid, signal);
    }
    close(engine->to);
    close(engine->from);
    waitpid(engine->pid, NULL, 0);
    engine->pid = -1;
}

static int write_text_file(const char *path, const char *text)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
    if (fd < 0) { return -errno; }
    int ok = write_all(fd, text, strlen(text));
    close(fd);
    return ok == 0 ? 0 : -EIO;
}

static int write_prompt(unsigned index) { return write_text_file(PROMPT_FILE, k6_engine_prompt[index]); }

/* One request frame: Thalyx's engine takes the prompt and the grammar as
 * paths, so the grammar, when there is one, is a file too. */
static int send_request(Engine *engine, unsigned index, const char *grammar_path)
{
    uint32_t predict = k6_engine_predict[index];
    uint64_t seed = 0;
    uint32_t path_len = (uint32_t)strlen(PROMPT_FILE);
    uint32_t grammar_len = (uint32_t)strlen(grammar_path);
    if (write_all(engine->to, "THQ1", 4) != 0 || write_all(engine->to, &predict, 4) != 0
        || write_all(engine->to, &seed, 8) != 0 || write_all(engine->to, &path_len, 4) != 0
        || write_all(engine->to, PROMPT_FILE, path_len) != 0
        || write_all(engine->to, &grammar_len, 4) != 0
        || (grammar_len && write_all(engine->to, grammar_path, grammar_len) != 0)) {
        return -EPIPE;
    }
    return 0;
}

/* One request frame and its answer. The body of an answer is the prompt the
 * engine read followed by the completion; the digest is over the completion,
 * which is what the host compares with the reference. */
static int64_t ask(Engine *engine, unsigned index, uint64_t *digest, uint32_t *length)
{
    int sent = send_request(engine, index, "");
    if (sent != 0) { return sent; }
    uint8_t head[17];
    if (read_exactly(engine->from, head, sizeof(head)) != 0 || memcmp(head, "THA1", 4) != 0) {
        return -EPROTO;
    }
    uint8_t status = head[4];
    uint32_t body_len;
    memcpy(&body_len, head + 13, 4);
    static char body[65536];
    if (body_len > sizeof(body) || read_exactly(engine->from, body, body_len) != 0) { return -EPROTO; }
    if (status != 0) { return -(int64_t)(100 + status); }
    size_t prompt_len = strlen(k6_engine_prompt[index]);
    if (body_len < prompt_len) { return -EPROTO; }
    *digest = bench_fnv1a(body + prompt_len, body_len - prompt_len);
    *length = (uint32_t)(body_len - prompt_len);
    return 0;
}

static uint32_t to_us(uint64_t cycles)
{
    uint64_t us = (uint64_t)(((unsigned __int128)cycles * 1000000u) / bench_tsc_hz());
    return us >= K6_LOST_SAMPLE ? (uint32_t)(K6_LOST_SAMPLE - 1) : (uint32_t)us;
}

static int64_t engine_load(uint32_t *out, uint32_t n)
{
    for (uint32_t i = 0; i < n; i++) {
        Engine engine = {-1, -1, -1};
        uint64_t c0 = bench_cycles();
        int64_t status = engine_start(&engine);
        uint64_t c1 = bench_cycles();
        engine_stop(&engine, 0);
        if (status != 0) { return status; }
        out[i] = to_us(c1 - c0);
    }
    return (int64_t)n;
}

static int64_t engine_infer(uint32_t index, uint32_t *out, uint32_t n)
{
    if (index >= K6_ENGINE_PROMPTS) { return -EINVAL; }
    if (resident.pid <= 0) {
        int64_t status = engine_start(&resident);
        if (status != 0) { return status; }
    }
    int64_t status = write_prompt(index);
    if (status != 0) { return status; }
    uint64_t first = 0;
    uint32_t first_length = 0, same = 0, differ = 0, errors = 0;
    for (uint32_t i = 0; i < n; i++) {
        uint64_t digest = 0;
        uint32_t length = 0;
        uint64_t c0 = bench_cycles();
        status = ask(&resident, index, &digest, &length);
        uint64_t c1 = bench_cycles();
        if (status != 0) {
            if (errors++ == 0) {
                plat_emit(K6_NOTE_ERROR, K6_BENCH_ENGINE_INFER | ((uint64_t)(uint32_t)status << 32));
            }
            out[i] = (uint32_t)K6_LOST_SAMPLE;
            continue;
        }
        out[i] = to_us(c1 - c0);
        if (same + differ == 0) {
            first = digest;
            first_length = length;
        }
        if (digest == first) { same++; } else { differ++; }
    }
    plat_emit(K6_NOTE_AUX, K6_BENCH_ENGINE_INFER | ((uint64_t)index << 16) | ((uint64_t)first_length << 32));
    plat_emit(K6_NOTE_DIGEST, first);
    plat_emit(K6_NOTE_CHECK, K6_BENCH_ENGINE_INFER | ((uint64_t)same << 32));
    if (differ) { plat_emit(K6_NOTE_MISMATCH, index); }
    return (int64_t)n;
}

/* Nanoseconds the engine has run, summed over its threads from the
 * scheduler's own accounting: the same trigger the native side uses, which
 * is the processor time charged to the request, not a wall-clock delay. The
 * first version read the leader's line alone, and the engine computes on a
 * thread of its own: the trigger never fired and each sample waited out its
 * whole minute. */
static uint64_t run_ns(pid_t pid)
{
    char path[96], buffer[128];
    snprintf(path, sizeof(path), "/proc/%d/task", (int)pid);
    DIR *dir = opendir(path);
    if (dir == NULL) { return 0; }
    uint64_t total = 0;
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        if (entry->d_name[0] == '.') { continue; }
        snprintf(path, sizeof(path), "/proc/%d/task/%s/schedstat", (int)pid, entry->d_name);
        int fd = open(path, O_RDONLY | O_CLOEXEC);
        if (fd < 0) { continue; }
        ssize_t n = read(fd, buffer, sizeof(buffer) - 1);
        close(fd);
        if (n <= 0) { continue; }
        buffer[n] = 0;
        total += strtoull(buffer, NULL, 10);
    }
    closedir(dir);
    return total;
}

/* The cancellation the Linux profile declares: a signal to the process. The
 * engine is killed once it has computed ten milliseconds for a long request,
 * and the sample is the time from the signal to the next answer, which needs
 * the engine started and its weights loaded again. */
static int64_t engine_cancel(uint32_t *out, uint32_t n)
{
    for (uint32_t i = 0; i < n; i++) {
        if (resident.pid <= 0) {
            int64_t status = engine_start(&resident);
            if (status != 0) { return status; }
        }
        /* Four hundred tokens under a grammar that never accepts an end: the
         * fixture's long prompt ends after a few tokens on this model, and a
         * request that has already been answered cannot be cancelled. */
        int64_t status = write_prompt(K6_ENGINE_PROMPTS - 1);
        if (status == 0) { status = write_text_file(GRAMMAR_FILE, K6_CANCEL_GRAMMAR); }
        if (status != 0) { return status; }
        uint64_t before = run_ns(resident.pid);
        status = send_request(&resident, K6_ENGINE_PROMPTS - 1, GRAMMAR_FILE);
        if (status != 0) { return status; }
        uint64_t deadline = plat_now_ns() + 60000000000ull;
        while (run_ns(resident.pid) < before + 10000000ull && plat_now_ns() < deadline) {
            struct timespec wait = {0, 100000};
            nanosleep(&wait, NULL);
        }
        uint64_t c0 = bench_cycles();
        engine_stop(&resident, SIGKILL);
        status = engine_start(&resident);
        if (status != 0) { return status; }
        status = write_prompt(0);
        uint64_t digest = 0;
        uint32_t length = 0;
        if (status == 0) { status = ask(&resident, 0, &digest, &length); }
        uint64_t c1 = bench_cycles();
        if (status != 0) { return status; }
        out[i] = to_us(c1 - c0);
    }
    return (int64_t)n;
}

int64_t plat_special(uint32_t bench, uint32_t param, uint32_t *out, uint32_t max)
{
    switch (bench) {
    case K6_BENCH_SCHED_WAKE:
        return bench_wake(out, max);
    case K6_BENCH_SCALE_COMPUTE:
        return bench_scale_compute(param, out, max);
    case K6_BENCH_SCALE_IPC:
        return bench_scale_ipc(param, out, max);
    case K6_BENCH_QUOTA_SHARE: {
        int64_t status = quota_share(param);
        return status < 0 ? status : 0;
    }
    case K6_BENCH_CLOSURE_UNIT:
        return closure_unit(out, max);
    case K6_BENCH_ENGINE_LOAD:
        return engine_load(out, max);
    case K6_BENCH_ENGINE_INFER:
        return engine_infer(param, out, max);
    case K6_BENCH_ENGINE_CANCEL:
        return engine_cancel(out, max);
    default:
        return -ENOSYS;
    }
}

/* ------------------------------------------------------------------ init */

static void describe_guest(void)
{
    struct utsname name;
    if (uname(&name) == 0) {
        text("kernel_release", name.release);
        text("kernel_version", name.version);
    }
    char buffer[512];
    int fd = open("/proc/cmdline", O_RDONLY);
    if (fd >= 0) {
        ssize_t n = read(fd, buffer, sizeof(buffer) - 1);
        close(fd);
        if (n > 0) {
            buffer[n] = 0;
            if (buffer[n - 1] == '\n') { buffer[n - 1] = 0; }
            text("cmdline", buffer);
        }
    }
    DIR *dir = opendir("/sys/devices/system/cpu/vulnerabilities");
    if (dir) {
        struct dirent *entry;
        while ((entry = readdir(dir)) != NULL) {
            if (entry->d_name[0] == '.') { continue; }
            char path[320];
            snprintf(path, sizeof(path), "/sys/devices/system/cpu/vulnerabilities/%s", entry->d_name);
            int vfd = open(path, O_RDONLY);
            if (vfd < 0) { continue; }
            ssize_t n = read(vfd, buffer, sizeof(buffer) - 1);
            close(vfd);
            if (n <= 0) { continue; }
            buffer[n] = 0;
            if (buffer[n - 1] == '\n') { buffer[n - 1] = 0; }
            char line[480];
            snprintf(line, sizeof(line), "%s %s", entry->d_name, buffer);
            text("vulnerability", line);
        }
        closedir(dir);
    }
    snprintf(buffer, sizeof(buffer), "%ld", sysconf(_SC_NPROCESSORS_ONLN));
    text("processors_online", buffer);
}

int main(void)
{
    mkdir("/dev", 0755);
    mount("devtmpfs", "/dev", "devtmpfs", 0, NULL);
    mkdir("/proc", 0755);
    mount("proc", "/proc", "proc", 0, NULL);
    mkdir("/sys", 0755);
    mount("sysfs", "/sys", "sysfs", 0, NULL);
    mount("cgroup2", "/sys/fs/cgroup", "cgroup2", 0, NULL);
    mkdir("/tmp", 01777);
    mount("tmpfs", "/tmp", "tmpfs", 0, NULL);
    int serial = open("/dev/ttyS0", O_WRONLY | O_NOCTTY | O_CLOEXEC);
    if (serial >= 0) { out_fd = serial; }
    /* PID 1 holds the privilege; a guest without the debug console device
     * still reports, slowly, through the tty. */
    debugcon = ioperm(DEBUGCON_PORT, 1, 1) == 0;
    signal(SIGPIPE, SIG_IGN);

    text("backend", "linux");
    text("report_channel", debugcon ? "debugcon" : "ttyS0");
    describe_guest();

    static k6_plan plan;
    int fd = open("/plan.bin", O_RDONLY);
    if (fd < 0 || read_exactly(fd, &plan, sizeof(plan)) != 0) {
        text("error", "no plan");
    } else {
        close(fd);
        start_server();
        bench_run_plan(&plan);
    }
    engine_stop(&resident, SIGKILL);
    if (server_pid > 0) {
        kill(server_pid, SIGKILL);
        waitpid(server_pid, NULL, 0);
    }
    text("end", "power_off");
    sync();
    reboot(RB_POWER_OFF);
    return 0;
}
