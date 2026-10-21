#ifndef DROST_PROCESS_ACTIVITY_H
#define DROST_PROCESS_ACTIVITY_H

#include <errno.h>
#include <limits.h>
#include <stdint.h>
#include <stdbool.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#if defined(__APPLE__)
#include <libproc.h>
#include <sys/proc.h>
#include <sys/proc_info.h>
#elif defined(__linux__)
#include <fcntl.h>
#include <string.h>

/* Fixed-size paths and reads keep the background query allocation-free. */
static char *drost_process_pid_text(char *out, uint32_t pid) {
    char digits[10];
    unsigned count = 0;
    do {
        digits[count++] = (char)('0' + pid % 10);
        pid /= 10;
    } while (pid);
    while (count) *out++ = digits[--count];
    return out;
}

static ssize_t drost_process_read(const char *path, char *buffer, size_t capacity) {
    int fd;
    do {
        fd = open(path, O_RDONLY | O_CLOEXEC);
    } while (fd < 0 && errno == EINTR);
    if (fd < 0) return -1;
    ssize_t size;
    do {
        size = read(fd, buffer, capacity);
    } while (size < 0 && errno == EINTR);
    close(fd);
    return size;
}
#endif

/* Compare the launched executable's file identity, preserving symlink aliases.
 * An exec(2) replacement must not be mistaken for a shell waiting for input. */
static int drost_process_matches_program(uint32_t pid, const char *expected_program) {
    if (!pid || pid > INT_MAX || !expected_program || !*expected_program) return -1;
    struct stat expected, actual;
    if (stat(expected_program, &expected) != 0) return -1;
#if defined(__APPLE__)
    char path[PROC_PIDPATHINFO_MAXSIZE];
    if (proc_pidpath((int)pid, path, sizeof(path)) <= 0) return -1;
#elif defined(__linux__)
    char path[64] = "/proc/";
    char *end = drost_process_pid_text(path + 6, pid);
    memcpy(end, "/exe", sizeof("/exe"));
#else
    return -1;
#endif
#if defined(__APPLE__) || defined(__linux__)
    if (stat(path, &actual) != 0) return -1;
    if (expected.st_dev == actual.st_dev && expected.st_ino == actual.st_ino) return 1;
#if defined(__APPLE__)
    /* macOS /bin/sh is an executable dispatcher, not a symlink. Its system
     * selection points to the actual shell image; compare identities so
     * aliases of /bin/sh behave identically without guessing shell names. */
    struct stat dispatcher, selected;
    if (stat("/bin/sh", &dispatcher) == 0 &&
        expected.st_dev == dispatcher.st_dev && expected.st_ino == dispatcher.st_ino &&
        stat("/private/var/select/sh", &selected) == 0)
        return selected.st_dev == actual.st_dev && selected.st_ino == actual.st_ino;
#endif
    return 0;
#endif
}

static int drost_process_idle_activity(uint32_t pid, const char *expected_program) {
    int same = drost_process_matches_program(pid, expected_program);
    return same == 1 ? 0 : same == 0 ? 1 : -1;
}

/* 0 = observed idle shell, 1 = work, -1 = unknown. Call off the UI thread.
 * login_wrapper is set only when Ghostty constructed its macOS login(1)
 * invocation. Otherwise root is the shell itself; no shell-name guessing,
 * integration, heap allocation, or timed sampling is used. */
static int drost_process_activity(
    uint32_t root, uint32_t foreground, bool login_wrapper, const char *expected_program
) {
    if (!root || !foreground || root > INT_MAX || foreground > INT_MAX) return -1;

#if defined(__APPLE__)
    if (login_wrapper) {
        /* The setuid login wrapper denies task-info queries to its own parent.
         * Its process group and child relationship remain inspectable. */
        if (getpgid((pid_t)root) != (pid_t)root) return -1;
        /* login(1) waits for its shell in a separate foreground group.
         * Resolve exactly that child, not whichever foreground job is running. */
        pid_t shells[2] = {0};
        errno = 0;
        int count = proc_listchildpids((pid_t)root, shells, sizeof(shells));
        if (count < 0 || (count == 0 && errno != 0)) return -1;
        if (count == 0) return -1;
        if (count != 1) return 1;
        struct proc_bsdinfo shell;
        if (proc_pidinfo(shells[0], PROC_PIDTBSDINFO, 0, &shell, sizeof(shell)) !=
            (int)sizeof(shell) || shell.pbi_ppid != root || shell.pbi_pid != (uint32_t)shells[0])
            return -1;
        return drost_process_activity(shell.pbi_pid, foreground, false, expected_program);
    }
    struct proc_taskallinfo info;
    if (proc_pidinfo((int)root, PROC_PIDTASKALLINFO, 0, &info, sizeof(info)) !=
        (int)sizeof(info)) return -1;
    if (info.pbsd.pbi_pid != root ||
        info.pbsd.pbi_status == SZOMB || (info.pbsd.pbi_flags & PROC_FLAG_INEXIT))
        return -1;
    /* A shell may restore login(1)'s process group before exec(2). */
    if (info.pbsd.pbi_pgid != root)
        return drost_process_matches_program(root, expected_program) == 0 ? 1 : -1;
    if (foreground != root) return 1;

    /* Only existence matters, so truncating the child list is harmless.
     * libproc maps errors to zero, retaining errno; clear it first to
     * distinguish an empty successful result from an inspection failure. */
    pid_t children[16];
    errno = 0;
    int count = proc_listchildpids((pid_t)root, children, sizeof(children));
    if (count < 0 || (count == 0 && errno != 0)) return -1;
    if (count > 0) return 1;

    struct proc_taskallinfo after;
    if (proc_pidinfo((int)root, PROC_PIDTASKALLINFO, 0, &after, sizeof(after)) !=
        (int)sizeof(after)) return -1;
    if (after.pbsd.pbi_pid != root || after.pbsd.pbi_pgid != root ||
        after.pbsd.pbi_start_tvsec != info.pbsd.pbi_start_tvsec ||
        after.pbsd.pbi_start_tvusec != info.pbsd.pbi_start_tvusec ||
        after.pbsd.pbi_status == SZOMB || (after.pbsd.pbi_flags & PROC_FLAG_INEXIT))
        return -1;
    if (after.pbsd.pbi_status == SSTOP) return 1;
    if (after.pbsd.pbi_status != SRUN && after.pbsd.pbi_status != SSLEEP) return -1;
    /* BSD SRUN alone also describes sleeping macOS tasks. The task's actual
     * runnable-thread count distinguishes an idle read from a busy builtin. */
    if (after.ptinfo.pti_threadnum <= 0 || after.ptinfo.pti_numrunning < 0) return -1;
    return after.ptinfo.pti_numrunning > 0 ? 1 : drost_process_idle_activity(root, expected_program);
#elif defined(__linux__)
    if (login_wrapper) return -1;
    if (getpgid((pid_t)root) != (pid_t)root)
        return drost_process_matches_program(root, expected_program) == 0 ? 1 : -1;
    char path[64] = "/proc/";
    char *end = drost_process_pid_text(path + 6, root);
    memcpy(end, "/stat", sizeof("/stat"));
    char status[4096];
    ssize_t count = drost_process_read(path, status, sizeof(status) - 1);
    if (count <= 0 || count == (ssize_t)sizeof(status) - 1) return -1;
    status[count] = 0;
    /* comm may contain spaces and parentheses; its final ')' delimits state. */
    const char *comm_end = strrchr(status, ')');
    if (!comm_end || strlen(comm_end) < 4 || comm_end[1] != ' ' || comm_end[3] != ' ')
        return -1;
    if (getpgid((pid_t)root) != (pid_t)root) return -1;
    switch (comm_end[2]) {
        case 'S': break; /* interruptible sleep: ordinary idle terminal read */
        case 'R': /* running/runnable, including shell builtins */
        case 'D': /* blocked work */
        case 'T': /* stopped shell */
        case 't': return 1; /* traced shell */
        default: return -1; /* zombie, dead, or an unrecognized state */
    }
    if (foreground != root) return 1;

    end = drost_process_pid_text(path + 6, root);
    memcpy(end, "/task/", 6);
    end = drost_process_pid_text(end + 6, root);
    memcpy(end, "/children", sizeof("/children"));
    char children[64];
    count = drost_process_read(path, children, sizeof(children));
    if (count < 0) return -1;
    for (ssize_t i = 0; i < count; ++i) {
        if (children[i] >= '0' && children[i] <= '9') return 1;
        if (children[i] != ' ' && children[i] != '\t' && children[i] != '\n') return -1;
    }
    if (count == (ssize_t)sizeof(children)) return -1;
    return getpgid((pid_t)root) == (pid_t)root
        ? drost_process_idle_activity(root, expected_program) : -1;
#else
    return -1;
#endif
}

#endif
