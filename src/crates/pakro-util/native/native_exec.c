#define _GNU_SOURCE
#include <dirent.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <spawn.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern char **environ;

enum { MESSAGE_LIMIT = 8192 };
static _Atomic unsigned long request_counter;

static uint64_t monotonic_ms(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now)) return 0;
    return (uint64_t)now.tv_sec * 1000 + (uint64_t)now.tv_nsec / 1000000;
}

static int absolute_path(const char *path, char result[MESSAGE_LIMIT]) {
    if (!path || !path[0]) { errno = ENOENT; return -1; }
    if (path[0] == '/') {
        if (strlen(path) >= MESSAGE_LIMIT - 2) { errno = ENAMETOOLONG; return -1; }
        strcpy(result, path);
    } else {
        if (!getcwd(result, MESSAGE_LIMIT)) return -1;
        size_t used = strlen(result), length = strlen(path);
        if (used + length + 1 >= MESSAGE_LIMIT - 2) { errno = ENAMETOOLONG; return -1; }
        result[used++] = '/';
        memcpy(result + used, path, length + 1);
    }
    return 0;
}

/* Only file messages cross the worker boundary; no socket permission is needed. */
static int request_path(char kind, const char *path, char sealed[MESSAGE_LIMIT]) {
    const char *requests = getenv("PAKRO_NATIVE_REQUESTS");
    const char *responses = getenv("PAKRO_NATIVE_RESPONSES");
    const char *duration = getenv("PAKRO_NATIVE_DEADLINE_MS");
    uint64_t timeout = 10000;
    char absolute[MESSAGE_LIMIT], stem[96], temporary[112], request[112], response[112];
    char message[MESSAGE_LIMIT];
    int request_dir = -1, response_dir = -1, file = -1, result = -1, temporary_owned = 0;
    if (!requests || requests[0] != '/' || !responses || responses[0] != '/') {
        errno = EACCES; return -1;
    }
    if (duration) {
        char *end;
        errno = 0;
        unsigned long long parsed = strtoull(duration, &end, 10);
        if (errno || !duration[0] || *end || !parsed || parsed > UINT64_MAX / 2) {
            errno = EACCES; return -1;
        }
        timeout = (uint64_t)parsed;
    }
    if (absolute_path(path, absolute)) return -1;
    size_t length = strlen(absolute);
    message[0] = kind; message[1] = '\0';
    memcpy(message + 2, absolute, length);
    temporary[0] = '\0';
    uint64_t started = monotonic_ms();
    if (!started) { errno = EACCES; return -1; }
    request_dir = open(requests, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (request_dir < 0) goto done;
    response_dir = open(responses, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (response_dir < 0) goto done;
    /* An exec keeps its PID but reloads this counter. Do not reuse its old mailbox. */
    struct timespec serial;
    if (clock_gettime(CLOCK_MONOTONIC, &serial)) goto done;
    unsigned long empty = 0;
    unsigned long seed = (unsigned long)serial.tv_sec * 1000000000UL + (unsigned long)serial.tv_nsec;
    atomic_compare_exchange_strong_explicit(&request_counter, &empty, seed ? seed : 1,
                                            memory_order_relaxed, memory_order_relaxed);
    for (;;) {
        uint64_t now = monotonic_ms();
        if (!now || now - started >= timeout) { errno = EACCES; goto done; }
        snprintf(stem, sizeof(stem), "%ld-%lu", (long)getpid(),
                 atomic_fetch_add_explicit(&request_counter, 1, memory_order_relaxed));
        snprintf(temporary, sizeof(temporary), "%s.tmp", stem);
        snprintf(request, sizeof(request), "%s.req", stem);
        snprintf(response, sizeof(response), "%s.resp", stem);
        struct stat previous;
        if (fstatat(request_dir, request, &previous, AT_SYMLINK_NOFOLLOW) == 0) continue;
        if (errno != ENOENT) goto done;
        if (fstatat(response_dir, response, &previous, AT_SYMLINK_NOFOLLOW) == 0) continue;
        if (errno != ENOENT) goto done;
        file = openat(request_dir, temporary, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
        if (file >= 0) { temporary_owned = 1; break; }
        if (errno != EEXIST) goto done;
        if (monotonic_ms() - started >= timeout) { errno = EACCES; goto done; }
    }
    for (size_t written = 0; written < length + 2;) {
        ssize_t count = write(file, message + written, length + 2 - written);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) goto done;
        written += (size_t)count;
    }
    if (close(file)) { file = -1; goto done; }
    file = -1;
    if (renameat(request_dir, temporary, request_dir, request)) goto done;
    for (;;) {
        file = openat(response_dir, response, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC);
        if (file >= 0) break;
        if (errno != ENOENT && errno != EINTR) goto done;
        uint64_t now = monotonic_ms();
        if (!now || now - started >= timeout) { errno = EACCES; goto done; }
        struct timespec pause = { .tv_sec = 0, .tv_nsec = 10000000 };
        nanosleep(&pause, NULL);
    }
    struct stat info;
    if (fstat(file, &info) || !S_ISREG(info.st_mode) || info.st_size < 1 || info.st_size > MESSAGE_LIMIT) {
        errno = EACCES; goto done;
    }
    size_t received = 0;
    while (received < (size_t)info.st_size) {
        ssize_t count = read(file, message + received, (size_t)info.st_size - received);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) { errno = EACCES; goto done; }
        received += (size_t)count;
    }
    if (message[0] == 2 && kind == 'R' && received == 1) { result = 2; goto done; }
    if (message[0] != 0 || received < 2 || message[1] != '/' ||
        received >= MESSAGE_LIMIT || memchr(message + 1, '\0', received - 1)) {
        errno = EACCES; goto done;
    }
    memcpy(sealed, message + 1, received - 1);
    sealed[received - 1] = '\0';
    result = 0;
done:;
    int saved = errno;
    if (file >= 0) close(file);
    if (request_dir >= 0) {
        if (temporary_owned) unlinkat(request_dir, temporary, 0);
        close(request_dir);
    }
    if (response_dir >= 0) close(response_dir);
    errno = saved;
    return result;
}

static int executable_path(const char *file, int search, char resolved[MESSAGE_LIMIT]) {
    char absolute[MESSAGE_LIMIT];
    if (!file || !file[0]) { errno = ENOENT; return -1; }
    if (!search || strchr(file, '/')) {
        /* Canonical qualification belongs to the watcher request entrance.
         * libc realpath also probes '/' metadata, outside these input grants. */
        if(absolute_path(file, absolute))return -1;
        strcpy(resolved,absolute);return 0;
    }
    const char *paths = getenv("PATH");
    if (!paths) paths = "/usr/bin:/bin";
    int denied = 0;
    for (const char *next = paths;;) {
        const char *end = strchr(next, ':');
        size_t count = end ? (size_t)(end - next) : strlen(next);
        if (count + strlen(file) + 2 < sizeof(absolute)) {
            if (count) { memcpy(absolute, next, count); absolute[count++] = '/'; }
            memcpy(absolute + count, file, strlen(file) + 1);
            if (access(absolute, X_OK) == 0) { strcpy(resolved,absolute); return 0; }
            if (errno == EACCES) denied = 1;
        }
        if (!end) break;
        next = end + 1;
    }
    errno = denied ? EACCES : ENOENT;
    return -1;
}

#ifndef PAKRO_NATIVE_WRAPPER

static int gated_path(const char *file, int search, char sealed[MESSAGE_LIMIT]) {
    char resolved[MESSAGE_LIMIT];
    if (executable_path(file, search, resolved)) return -1;
    return request_path('E', resolved, sealed);
}

static int gate_execve(const char *path, char *const argv[], char *const envp[]) {
    char sealed[MESSAGE_LIMIT];
    if (gated_path(path, 0, sealed)) return -1;
#ifdef __APPLE__
    /* dyld excludes this image's direct original binding from interposition.
     * RTLD_NEXT lookup is interposed again on Darwin and would recurse. */
    return execve(sealed, argv, envp);
#else
    int (*original)(const char *, char *const[], char *const[]) = dlsym(RTLD_NEXT, "execve");
    if (!original) { errno = EACCES; return -1; }
    return original(sealed, argv, envp);
#endif
}

static int gate_execv(const char *path, char *const argv[]) {
    return gate_execve(path, argv, environ);
}

static int gate_execvp(const char *path, char *const argv[]) {
    char sealed[MESSAGE_LIMIT];
    if (gated_path(path, 1, sealed)) return -1;
#ifdef __APPLE__
    return execvp(sealed, argv);
#else
    int (*original)(const char *, char *const[]) = dlsym(RTLD_NEXT, "execvp");
    if (!original) { errno = EACCES; return -1; }
    return original(sealed, argv);
#endif
}

static int gate_posix_spawn(pid_t *pid, const char *path, const posix_spawn_file_actions_t *actions,
                            const posix_spawnattr_t *attributes, char *const argv[], char *const envp[]) {
    char sealed[MESSAGE_LIMIT];
    if (gated_path(path, 0, sealed)) return errno;
#ifdef __APPLE__
    return posix_spawn(pid, sealed, actions, attributes, argv, envp);
#else
    int (*original)(pid_t *, const char *, const posix_spawn_file_actions_t *,
                    const posix_spawnattr_t *, char *const[], char *const[]) = dlsym(RTLD_NEXT, "posix_spawn");
    if (!original) return EACCES;
    return original(pid, sealed, actions, attributes, argv, envp);
#endif
}

static int gate_posix_spawnp(pid_t *pid, const char *path, const posix_spawn_file_actions_t *actions,
                             const posix_spawnattr_t *attributes, char *const argv[], char *const envp[]) {
    char sealed[MESSAGE_LIMIT];
    if (gated_path(path, 1, sealed)) return errno;
#ifdef __APPLE__
    return posix_spawnp(pid, sealed, actions, attributes, argv, envp);
#else
    int (*original)(pid_t *, const char *, const posix_spawn_file_actions_t *,
                    const posix_spawnattr_t *, char *const[], char *const[]) = dlsym(RTLD_NEXT, "posix_spawn");
    if (!original) return EACCES;
    return original(pid, sealed, actions, attributes, argv, envp);
#endif
}

#ifdef __APPLE__
#define INTERPOSE(replacement, original) \
    __attribute__((used)) static struct { const void *replace; const void *target; } \
    interpose_##original __attribute__((section("__DATA,__interpose"))) = { \
        (const void *)(uintptr_t)&replacement, (const void *)(uintptr_t)&original \
    }
INTERPOSE(gate_execve, execve);
INTERPOSE(gate_execv, execv);
INTERPOSE(gate_execvp, execvp);
INTERPOSE(gate_posix_spawn, posix_spawn);
INTERPOSE(gate_posix_spawnp, posix_spawnp);
#else
int execve(const char *path, char *const argv[], char *const envp[]) { return gate_execve(path, argv, envp); }
int execv(const char *path, char *const argv[]) { return gate_execv(path, argv); }
int execvp(const char *path, char *const argv[]) { return gate_execvp(path, argv); }
int posix_spawn(pid_t *pid, const char *path, const posix_spawn_file_actions_t *actions,
                const posix_spawnattr_t *attributes, char *const argv[], char *const envp[]) {
    return gate_posix_spawn(pid, path, actions, attributes, argv, envp);
}
int posix_spawnp(pid_t *pid, const char *path, const posix_spawn_file_actions_t *actions,
                 const posix_spawnattr_t *attributes, char *const argv[], char *const envp[]) {
    return gate_posix_spawnp(pid, path, actions, attributes, argv, envp);
}
#endif

#else

int main(int argc, char **argv) {
    const char *base = strrchr(argv[0], '/');
    base = base ? base + 1 : argv[0];
    int rust = strstr(base, "rustc") != NULL;
    int cxx = strstr(base, "++") != NULL || !strcmp(base, "cxx");
    const char *real = getenv(rust ? "PAKRO_NATIVE_REAL_RUSTC" :
                              cxx ? "PAKRO_NATIVE_REAL_CXX" : "PAKRO_NATIVE_REAL_CC");
    if (!real || real[0] != '/') {
        fprintf(stderr, "pakro: fixed compiler is missing for %s\n", base); return 126;
    }
    const char *gate = getenv("PAKRO_NATIVE_GATE_LIBRARY");
    if (!gate || gate[0] != '/' || strlen(gate) > MESSAGE_LIMIT) {
        fprintf(stderr, "pakro: fixed native exec gate is missing\n"); return 126;
    }
#ifdef __APPLE__
    if (setenv("DYLD_INSERT_LIBRARIES", gate, 1)) { perror("pakro native gate environment"); return 126; }
#else
    if (setenv("LD_PRELOAD", gate, 1)) { perror("pakro native gate environment"); return 126; }
#endif
    const char *fixed = getenv(rust ? "PAKRO_NATIVE_RUSTC_ARGS" :
                               cxx ? "PAKRO_NATIVE_CXX_ARGS" : "PAKRO_NATIVE_CC_ARGS");
    if (fixed && fixed[0]) {
        if (strlen(fixed) > MESSAGE_LIMIT || argc > INT_MAX - 129) {
            fprintf(stderr, "pakro: fixed compiler arguments exceed their budget\n"); return 126;
        }
        char *storage = strdup(fixed);
        char **combined = calloc((size_t)argc + 129, sizeof(char *));
        if (!storage || !combined) { perror("pakro fixed compiler arguments"); return 126; }
        combined[0] = argv[0];
        size_t used = 1;
        char *next = storage;
        for (;;) {
            if (used > 128) { fprintf(stderr, "pakro: too many fixed compiler arguments\n"); return 126; }
            char *end = strchr(next, '\x1f');
            if (end) *end = '\0';
            combined[used++] = next;
            if (!end) break;
            next = end + 1;
        }
        for (int at = 1; at < argc; at++) combined[used++] = argv[at];
        argc = (int)used;
        argv = combined;
    }
    const char *output = NULL, *out_dir = NULL, *crate = NULL, *extra = "";
    for (int at = 1; at < argc; at++) {
        const char *arg = argv[at];
        if (!strcmp(arg, "-o") && at + 1 < argc) output = argv[++at];
        else if (!strncmp(arg, "-o", 2) && arg[2]) output = arg + 2;
        else if (!strcmp(arg, "--out-dir") && at + 1 < argc) out_dir = argv[++at];
        else if (!strncmp(arg, "--out-dir=", 10)) out_dir = arg + 10;
        else if (!strcmp(arg, "--crate-name") && at + 1 < argc) crate = argv[++at];
        else if (!strncmp(arg, "--crate-name=", 13)) crate = arg + 13;
        else if (!strncmp(arg, "-Cextra-filename=", 17)) extra = arg + 17;
        else if (!strcmp(arg, "-C") && at + 1 < argc && !strncmp(argv[at + 1], "extra-filename=", 15))
            extra = argv[++at] + 15;
    }
    char resolved[MESSAGE_LIMIT], sealed[MESSAGE_LIMIT];
    if (executable_path(real, 0, resolved) || request_path('E', resolved, sealed)) {
        fprintf(stderr, "pakro: fixed compiler execution was refused: %s\n", strerror(errno)); return 126;
    }
    const char *pool = getenv("PAKRO_NATIVE_EXEC_POOL");
    if (output && strcmp(output, "-") && pool && pool[0] == '/') {
        char target[MESSAGE_LIMIT];
        ssize_t length = readlink(output, target, sizeof(target));
        size_t prefix = strlen(pool);
        if (length > 0 && (size_t)length > prefix && !memcmp(target, pool, prefix) && target[prefix] == '/') {
            if (unlink(output)) { perror("pakro compiler output alias"); return 126; }
        }
    }
    pid_t child = fork();
    if (child < 0) { perror("pakro compiler fork"); return 126; }
    if (!child) {
        /* Preserve c++/clang++ driver selection even when REAL_CXX was canonicalized. */
        argv[0] = (char *)real;
        execve(sealed, argv, environ);
        perror("pakro fixed compiler exec"); _exit(126);
    }
    int status;
    while (waitpid(child, &status, 0) < 0) {
        if (errno == EINTR) continue;
        perror("pakro compiler wait"); return 126;
    }
    if (!WIFEXITED(status)) return WIFSIGNALED(status) ? 128 + WTERMSIG(status) : 126;
    if (WEXITSTATUS(status)) return WEXITSTATUS(status);
    if (output && strcmp(output, "-")) {
        if (request_path('R', output, sealed) < 0) {
            fprintf(stderr, "pakro: compiler output registration was refused: %s\n", strerror(errno)); return 126;
        }
    }
    if (rust && out_dir && crate) {
        char prefix[MESSAGE_LIMIT], normalized[MESSAGE_LIMIT];
        if (strlen(crate) + strlen(extra) + 1 >= sizeof(prefix)) return 126;
        strcpy(normalized, crate);
        for (char *at = normalized; *at; at++) if (*at == '-') *at = '_';
        snprintf(prefix, sizeof(prefix), "%s%s", normalized, extra);
        DIR *directory = opendir(out_dir);
        if (!directory) { perror("pakro Rust output directory"); return 126; }
        struct dirent *entry;
        while ((entry = readdir(directory))) {
            const char *name = entry->d_name;
            const char *candidate = !strncmp(name, "lib", 3) ? name + 3 : name;
            size_t prefix_length = strlen(prefix);
            if (strncmp(candidate, prefix, prefix_length) ||
                (candidate[prefix_length] && candidate[prefix_length] != '.')) continue;
            char path[MESSAGE_LIMIT];
            if (snprintf(path, sizeof(path), "%s/%s", out_dir, name) >= (int)sizeof(path)) {
                closedir(directory); return 126;
            }
            struct stat info;
            if (lstat(path, &info)) { perror("pakro Rust output"); closedir(directory); return 126; }
            if (!S_ISREG(info.st_mode)) continue;
            if (request_path('R', path, sealed) < 0) {
                fprintf(stderr, "pakro: Rust output registration was refused: %s\n", strerror(errno));
                closedir(directory); return 126;
            }
        }
        closedir(directory);
    }
    return 0;
}

#endif
