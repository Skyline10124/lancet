/* 复用父 Agent 提供的最小通知程序；本轮自行编译与测量，不复用此前数字。 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static void die(const char *m) { perror(m); exit(1); }

static int install_filter(void) {
    struct sock_filter f[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_openat, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[2])),
        BPF_JUMP(BPF_JMP | BPF_JSET | BPF_K, O_WRONLY | O_RDWR, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_USER_NOTIF),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog prog = {.len = sizeof f / sizeof f[0], .filter = f};
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)) die("no_new_privs");
    int fd = syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_NEW_LISTENER, &prog);
    if (fd < 0) die("seccomp");
    return fd;
}

static void send_fd(int sock, int fd) {
    char b = 0;
    struct iovec io = {&b, 1};
    union { char buf[CMSG_SPACE(sizeof(int))]; struct cmsghdr align; } u;
    struct msghdr m = {.msg_iov = &io, .msg_iovlen = 1, .msg_control = u.buf, .msg_controllen = sizeof u.buf};
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    c->cmsg_level = SOL_SOCKET; c->cmsg_type = SCM_RIGHTS; c->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(c), &fd, sizeof(int));
    if (sendmsg(sock, &m, 0) < 0) die("sendmsg");
}

static int recv_fd(int sock) {
    char b;
    struct iovec io = {&b, 1};
    union { char buf[CMSG_SPACE(sizeof(int))]; struct cmsghdr align; } u;
    struct msghdr m = {.msg_iov = &io, .msg_iovlen = 1, .msg_control = u.buf, .msg_controllen = sizeof u.buf};
    if (recvmsg(sock, &m, 0) <= 0) die("recvmsg");
    int fd; memcpy(&fd, CMSG_DATA(CMSG_FIRSTHDR(&m)), sizeof(int));
    return fd;
}

int main(int argc, char **argv) {
    if (argc < 3) { fprintf(stderr, "usage: %s native|cont|cont-read|addfd-read N [sync]\n", argv[0]); return 2; }
    const char *mode = argv[1];
    long n = atol(argv[2]);
    int sync = argc > 3 && strcmp(argv[3], "sync") == 0;
    int native = strcmp(mode, "native") == 0;
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv)) die("socketpair");
    pid_t pid = fork();
    if (pid < 0) die("fork");
    if (pid == 0) {
        close(sv[0]);
        if (!native) { int l = install_filter(); send_fd(sv[1], l); close(l); }
        char go; if (read(sv[1], &go, 1) != 1) die("read go");
        struct timespec a, b;
        clock_gettime(CLOCK_MONOTONIC, &a);
        for (long i = 0; i < n; i++) {
            int fd = open("/dev/null", O_WRONLY);
            if (fd < 0) die("open");
            close(fd);
        }
        clock_gettime(CLOCK_MONOTONIC, &b);
        double ns = (b.tv_sec - a.tv_sec) * 1e9 + (b.tv_nsec - a.tv_nsec);
        printf("%-11s %-6s %8.2f us/iter\n", mode, sync ? "sync" : "-", ns / n / 1000.0);
        fflush(stdout);
        _exit(0);
    }
    close(sv[1]);
    if (native) { if (write(sv[0], "g", 1) != 1) die("write go"); waitpid(pid, NULL, 0); return 0; }
    int lfd = recv_fd(sv[0]);
    if (sync && ioctl(lfd, SECCOMP_IOCTL_NOTIF_SET_FLAGS, SECCOMP_USER_NOTIF_FD_SYNC_WAKE_UP)) die("set_flags");
    struct seccomp_notif_sizes sz;
    if (syscall(SYS_seccomp, SECCOMP_GET_NOTIF_SIZES, 0, &sz)) die("sizes");
    struct seccomp_notif *req = calloc(1, sz.seccomp_notif);
    struct seccomp_notif_resp *resp = calloc(1, sz.seccomp_notif_resp);
    int do_read = strcmp(mode, "cont") != 0;
    int do_addfd = strcmp(mode, "addfd-read") == 0;
    if (write(sv[0], "g", 1) != 1) die("write go");
    char path[256];
    for (long i = 0; i < n; i++) {
        memset(req, 0, sz.seccomp_notif);
        if (ioctl(lfd, SECCOMP_IOCTL_NOTIF_RECV, req)) { if (errno == EINTR) { i--; continue; } die("recv"); }
        if (do_read) {
            struct iovec local = {path, sizeof path}, remote = {(void *)req->data.args[1], sizeof path};
            if (process_vm_readv(req->pid, &local, 1, &remote, 1, 0) < 0) die("process_vm_readv");
            path[sizeof path - 1] = 0;
            if (ioctl(lfd, SECCOMP_IOCTL_NOTIF_ID_VALID, &req->id)) die("id_valid");
        }
        if (do_addfd) {
            int nfd = open(path, (int)req->data.args[2] & ~O_CLOEXEC);
            if (nfd < 0) die("supervisor open");
            struct seccomp_notif_addfd ad = {.id = req->id, .flags = SECCOMP_ADDFD_FLAG_SEND, .srcfd = nfd};
            if (ioctl(lfd, SECCOMP_IOCTL_NOTIF_ADDFD, &ad) < 0) die("addfd");
            close(nfd);
        } else {
            memset(resp, 0, sz.seccomp_notif_resp);
            resp->id = req->id;
            resp->flags = SECCOMP_USER_NOTIF_FLAG_CONTINUE;
            if (ioctl(lfd, SECCOMP_IOCTL_NOTIF_SEND, resp)) die("send");
        }
    }
    waitpid(pid, NULL, 0);
    return 0;
}
