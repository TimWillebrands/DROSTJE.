#define _XOPEN_SOURCE 700
#define _DARWIN_C_SOURCE 1
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <poll.h>
#include <sched.h>
#include <signal.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#if defined(__APPLE__)
#include <pwd.h>
#endif

#include "../shim/process_activity.h"

/* Standalone: cc -std=c11 -Wall -Wextra -Werror process_activity.c -o probe
 * Run probe in an empty temporary directory; no shell integration is installed. */
static pid_t root_pid = -1;
static bool login_wrapper = false;
static pid_t shell_pid = -1;
static pid_t job_pid = -1;
static int terminal = -1;
static const char prompt[] = "__drost_prompt__ ";

static void cleanup(void) {
    if (job_pid > 0) {
        kill(-job_pid, SIGKILL);
        kill(job_pid, SIGKILL);
    }
    if (shell_pid > 0) {
        kill(-shell_pid, SIGKILL);
        kill(shell_pid, SIGKILL);
    }
    if (root_pid > 0) {
        kill(-root_pid, SIGKILL);
        kill(root_pid, SIGKILL);
    }
    /* A session leader can wait for unread terminal output while exiting. */
    if (terminal >= 0) {
        close(terminal);
        terminal = -1;
    }
    if (root_pid > 0) {
        while (waitpid(root_pid, NULL, 0) < 0 && errno == EINTR) {}
    }
}

static void fail(const char *message) {
    fprintf(stderr, "%s (errno %d: %s)\n", message, errno, strerror(errno));
    exit(1);
}

static int64_t milliseconds(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) fail("clock_gettime");
    return (int64_t)now.tv_sec * 1000 + now.tv_nsec / 1000000;
}

static void send_command(const char *command) {
    size_t left = strlen(command);
    while (left) {
        ssize_t n = write(terminal, command, left);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) fail("write command");
        command += n;
        left -= (size_t)n;
    }
}

static void marker_failure(const char *message, const char *marker, const char *scenario,
                           const char *output, size_t used, short events) {
    int saved_errno = errno;
    fprintf(stderr, "scenario: %s; marker: [%s]; root: %ld; shell: %ld; job: %ld; "
            "login wrapper: %d; foreground: %ld; poll events: 0x%x; received %zu bytes:\n",
            scenario, marker, (long)root_pid, (long)shell_pid, (long)job_pid,
            login_wrapper, (long)tcgetpgrp(terminal), (unsigned short)events, used);
    fwrite(output, 1, used, stderr);
    fputs("\n--- end terminal output ---\n", stderr);
    errno = saved_errno;
    fail(message);
}

static void await_marker(const char *marker, const char *scenario) {
    char output[32768] = {0};
    size_t used = 0;
    int64_t deadline = milliseconds() + 10000;
    while (!strstr(output, marker)) {
        int64_t remaining = deadline - milliseconds();
        if (remaining <= 0) {
            errno = ETIMEDOUT;
            marker_failure("terminal marker deadline", marker, scenario, output, used, 0);
        }
        struct pollfd fd = {terminal, POLLIN, 0};
        int ready = poll(&fd, 1, (int)remaining);
        if (ready < 0 && errno == EINTR) continue;
        if (ready == 0) {
            errno = ETIMEDOUT;
            marker_failure("terminal marker deadline", marker, scenario, output, used, fd.revents);
        }
        if (ready < 0)
            marker_failure("poll terminal marker", marker, scenario, output, used, fd.revents);
        ssize_t n = read(terminal, output + used, sizeof(output) - used - 1);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0)
            marker_failure("read terminal marker", marker, scenario, output, used, fd.revents);
        used += (size_t)n;
        output[used] = 0;
        if (used == sizeof(output) - 1)
            marker_failure("terminal output overflow", marker, scenario, output, used, fd.revents);
    }
}

/* Wait on the actual process observation, never an assumed settling delay. */
static void await_activity(int expected, int same_group, const char *scenario) {
    int64_t deadline = milliseconds() + 10000;
    int actual = -1;
    do {
        pid_t foreground = tcgetpgrp(terminal);
        if (foreground > 0 && (!same_group || foreground == shell_pid)) {
            actual = drost_process_activity((uint32_t)root_pid, (uint32_t)foreground,
                                            login_wrapper, "/bin/sh");
            if (actual == expected) {
                printf("PASS %s: activity=%d\n", scenario, actual);
                fflush(stdout);
                return;
            }
        }
        /* Yield the CPU; no sleep or timer is used as evidence of readiness. */
        sched_yield();
    } while (milliseconds() < deadline);
    fprintf(stderr, "%s: expected %d, got %d\n", scenario, expected, actual);
    fprintf(stderr, "root=%ld group=%ld shell=%ld group=%ld foreground=%ld shell-image=%d sleep-image=%d\n",
            (long)root_pid, (long)getpgid(root_pid), (long)shell_pid, (long)getpgid(shell_pid),
            (long)tcgetpgrp(terminal), drost_process_matches_program((uint32_t)shell_pid, "/bin/sh"),
            drost_process_matches_program((uint32_t)shell_pid, "/bin/sleep"));
#if defined(__APPLE__)
    pid_t children[2] = {0};
    int count = proc_listchildpids(root_pid, children, sizeof(children));
    fprintf(stderr, "root children=%d: %ld %ld\n", count, (long)children[0], (long)children[1]);
    struct proc_taskallinfo info;
    int bytes = proc_pidinfo(shell_pid, PROC_PIDTASKALLINFO, 0, &info, sizeof(info));
    fprintf(stderr, "shell task bytes=%d errno=%d\n", bytes, errno);
#endif
    fail("process activity deadline");
}

static void read_job_pid(void) {
    FILE *file = fopen("job.pid", "r");
    long pid = 0;
    if (!file || fscanf(file, "%ld", &pid) != 1 || pid <= 0 || pid > INT_MAX)
        fail("read shell job pid");
    fclose(file);
    job_pid = (pid_t)pid;
}

int main(int argc, char **argv) {
    atexit(cleanup);
    for (int i = 1; i < argc; ++i) {
        if (strcmp(argv[i], "--login-wrapper") == 0)
            login_wrapper = true;
        else
            fail("usage: process-activity [--login-wrapper]");
    }
#if defined(__APPLE__)
    char *username = NULL;
    if (login_wrapper) {
        struct passwd *user = getpwuid(getuid());
        if (!user || !user->pw_name || !user->pw_name[0]) fail("resolve login username");
        username = user->pw_name;
    }
#else
    if (login_wrapper) fail("login-wrapper regression requires macOS");
#endif
    terminal = posix_openpt(O_RDWR | O_NOCTTY);
    if (terminal < 0 || grantpt(terminal) || unlockpt(terminal)) fail("create pty");
    char *slave_name = ptsname(terminal);
    if (!slave_name) fail("pty slave name");
    root_pid = fork();
    if (root_pid < 0) fail("fork shell");
    if (root_pid == 0) {
        if (setsid() < 0) _exit(120);
        int slave = open(slave_name, O_RDWR);
        if (slave < 0 || ioctl(slave, TIOCSCTTY, 0) < 0) _exit(121);
        for (int fd = 0; fd < 3; ++fd) if (dup2(slave, fd) < 0) _exit(122);
        if (slave > 2) close(slave);
        close(terminal);
        char *args[] = {"sh", "-i", NULL};
        char *environment[] = {"PATH=/usr/bin:/bin", "HOME=.", "ENV=/dev/null",
            "PS1=__drost_prompt__ ", "PS2=__drost_continuation__ ",
            "TERM=dumb", "LC_ALL=C", NULL};
#if defined(__APPLE__)
        if (login_wrapper) {
            /* Match Exec.zig execCommand's direct-command argv, including
             * -l (preserve cwd) and -p (preserve the isolated environment). */
            char *login_args[] = {"login", "-flp", username, "/bin/sh", "-i", NULL};
            execve("/usr/bin/login", login_args, environment);
            _exit(126);
        }
#endif
        execve("/bin/sh", args, environment);
        _exit(123);
    }
    if (!login_wrapper) shell_pid = root_pid;
    await_marker(prompt, "initial unintegrated shell prompt");
    if (login_wrapper) {
        shell_pid = tcgetpgrp(terminal);
        if (shell_pid <= 0 || shell_pid == root_pid)
            fail("login must retain a distinct foreground shell child");
    }
    await_activity(0, 1, "idle unintegrated shell");

    send_command("sleep 600\n");
    int64_t deadline = milliseconds() + 10000;
    while ((job_pid = tcgetpgrp(terminal)) <= 0 || job_pid == shell_pid) {
        if (milliseconds() >= deadline) fail("foreground process group deadline");
        sched_yield();
    }
    await_activity(1, 0, "foreground sleep");
    /* Foreground ownership can precede exec and signal-handler setup.
     * SIGKILL makes fixture cleanup independent of that startup race. */
    if (kill(-job_pid, SIGKILL)) fail("terminate foreground sleep");
    await_marker(prompt, "prompt after foreground sleep");
    job_pid = -1;
    await_activity(0, 1, "returned prompt");

    send_command("sleep 600 & echo $! > job.pid\n");
    await_marker(prompt, "prompt after starting background sleep");
    read_job_pid();
    await_activity(1, 1, "background sleep at prompt");
    if (kill(job_pid, SIGKILL)) fail("terminate background sleep");
    send_command("wait; printf '__drost_reaped__\\n'\n");
    await_marker(prompt, "prompt after reaping background sleep");
    job_pid = -1;
    await_activity(0, 1, "background job reaped");

    send_command("sleep 600\n");
    deadline = milliseconds() + 10000;
    while ((job_pid = tcgetpgrp(terminal)) <= 0 || job_pid == shell_pid) {
        if (milliseconds() >= deadline) fail("stopped job process group deadline");
        sched_yield();
    }
    if (kill(-job_pid, SIGSTOP)) fail("stop foreground job");
    await_marker(prompt, "prompt after stopping foreground job");
    await_activity(1, 1, "stopped job at prompt");
    if (kill(-job_pid, SIGKILL)) fail("kill stopped job");
    send_command("wait; printf '__drost_reaped__\\n'\n");
    await_marker(prompt, "prompt after reaping stopped job");
    job_pid = -1;
    await_activity(0, 1, "stopped job reaped");

    send_command("printf '__drost_busy__\\n'; while :; do :; done\n");
    await_marker("__drost_busy__\r\n", "built-in loop entered");
    await_activity(1, 1, "shell built-in busy loop");
    /* SIGINT is delivered to the shell's foreground group, just like Ctrl-C. */
    if (kill(-shell_pid, SIGINT)) fail("interrupt built-in loop");
    await_marker(prompt, "prompt after interrupting built-in loop");
    await_activity(0, 1, "idle after built-in work");

    if (drost_process_activity(0, 0, false, "/bin/sh") != -1 ||
        drost_process_activity(UINT32_MAX, UINT32_MAX, false, "/bin/sh") != -1 ||
        drost_process_activity((uint32_t)root_pid, 0, login_wrapper, "/bin/sh") != -1)
        fail("invalid process identity must be unknown");
    pid_t gone = fork();
    if (gone < 0) fail("fork missing-pid fixture");
    if (gone == 0) _exit(0);
    while (waitpid(gone, NULL, 0) < 0) if (errno != EINTR) fail("reap fixture");
    if (drost_process_activity((uint32_t)gone, (uint32_t)gone, false, "/bin/sh") != -1)
        fail("missing process must be unknown");

    /* exec keeps the shell PID but can restore login's original process group.
     * Wait for the actual executable transition, not a guessed delay. */
    send_command("exec /bin/sleep 600\n");
    deadline = milliseconds() + 10000;
    int image_match;
    while ((image_match = drost_process_matches_program((uint32_t)shell_pid,
                                                       "/bin/sleep")) != 1) {
        if (milliseconds() >= deadline) {
            fprintf(stderr, "exec sleep image deadline: root %ld; shell %ld; match %d\n",
                    (long)root_pid, (long)shell_pid, image_match);
            fail("sleep executable identity deadline");
        }
        sched_yield();
    }
    await_activity(1, 0, "sleep replaced shell image without changing PID");
    puts("process activity regression scenarios passed");
    return 0;
}
