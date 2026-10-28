#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

/* Synthetic test protocol emitter. It never evaluates argv[6], executes a
 * Python script, or starts another process. Its real ELF/PT_INTERP remains
 * subject to Pakro's unchanged production musl verification. */
static int json_root(const char *root) {
    const unsigned char *p = (const unsigned char *)root;
    for (; *p; ++p) {
        if (*p == '"' || *p == '\\') {
            if (putchar('\\') == EOF || putchar(*p) == EOF) return -1;
        } else if (*p < 0x20) {
            if (printf("\\u%04x", (unsigned int)*p) < 0) return -1;
        } else if (putchar(*p) == EOF) return -1;
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 7 || strcmp(argv[1], "-E") || strcmp(argv[2], "-s") ||
        strcmp(argv[3], "-S") || strcmp(argv[4], "-B") || strcmp(argv[5], "-c")) return 93;
    char executable[PATH_MAX + 1];
    ssize_t length = readlink("/proc/self/exe", executable, PATH_MAX);
    if (length <= 0 || length >= PATH_MAX) return 94;
    executable[length] = '\0';
    const char suffix[] = "/bin/python";
    size_t suffix_size = sizeof(suffix) - 1;
    if ((size_t)length <= suffix_size || strcmp(executable + length - suffix_size, suffix)) return 94;
    executable[length - suffix_size] = '\0';
    int root = open(executable, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (root < 0) return 94;
    int fd = openat(root, ".pakro-probe.json", O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK);
    if (fd < 0) { close(root); return 95; }
    struct stat before, after;
    if (fstat(fd, &before) || !S_ISREG(before.st_mode) || before.st_uid != geteuid() ||
        before.st_nlink != 1 || (before.st_mode & 0022) || before.st_size <= 0 || before.st_size > 65536) {
        close(fd); close(root); return 95;
    }
    char document[65537];
    size_t used = 0;
    while (used < (size_t)before.st_size) {
        ssize_t count = read(fd, document + used, (size_t)before.st_size - used);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) { close(fd); close(root); return 95; }
        used += (size_t)count;
    }
    char tail;
    if (read(fd, &tail, 1) != 0 || fstat(fd, &after) || before.st_dev != after.st_dev ||
        before.st_ino != after.st_ino || before.st_size != after.st_size || before.st_mode != after.st_mode ||
        before.st_mtim.tv_sec != after.st_mtim.tv_sec || before.st_mtim.tv_nsec != after.st_mtim.tv_nsec ||
        before.st_ctim.tv_sec != after.st_ctim.tv_sec || before.st_ctim.tv_nsec != after.st_ctim.tv_nsec) {
        close(fd); close(root); return 95;
    }
    close(fd); close(root);
    if (memchr(document, '\0', used)) return 95;
    document[used] = '\0';
    const char token[] = "@ROOT@";
    char *start = document, *at;
    int replaced = 0;
    while ((at = strstr(start, token))) {
        if (fwrite(start, 1, (size_t)(at - start), stdout) != (size_t)(at - start) || json_root(executable)) return 95;
        start = at + sizeof(token) - 1;
        replaced = 1;
    }
    if (!replaced || fputs(start, stdout) == EOF || putchar('\n') == EOF || fflush(stdout)) return 95;
    return 0;
}
