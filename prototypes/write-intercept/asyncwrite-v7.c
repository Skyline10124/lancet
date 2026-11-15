/* Submit one large O_DIRECT write via io_uring or Linux AIO, then SIGKILL ourselves immediately. */
#define _GNU_SOURCE
#include <fcntl.h>
#include <linux/aio_abi.h>
#include <linux/io_uring.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc != 4) { fprintf(stderr, "usage: %s uring|aio file MiB\n", argv[0]); return 2; }
    size_t len = (size_t)atol(argv[3]) << 20;
    int fd = open(argv[2], O_RDWR | O_DIRECT);
    if (fd < 0) { perror("open"); return 1; }
    void *buf;
    if (posix_memalign(&buf, 4096, len)) { perror("posix_memalign"); return 1; }
    memset(buf, 0xAB, len);
    if (!strcmp(argv[1], "uring")) {
        struct io_uring_params p;
        memset(&p, 0, sizeof p);
        int ring = syscall(__NR_io_uring_setup, 4, &p);
        if (ring < 0) { perror("io_uring_setup"); return 1; }
        size_t sqsz = p.sq_off.array + p.sq_entries * sizeof(__u32);
        size_t cqsz = p.cq_off.cqes + p.cq_entries * sizeof(struct io_uring_cqe);
        if ((p.features & IORING_FEAT_SINGLE_MMAP) && cqsz > sqsz) sqsz = cqsz;
        char *sq = mmap(0, sqsz, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_POPULATE, ring, IORING_OFF_SQ_RING);
        struct io_uring_sqe *sqes = mmap(0, p.sq_entries * sizeof(struct io_uring_sqe), PROT_READ | PROT_WRITE,
                                         MAP_SHARED | MAP_POPULATE, ring, IORING_OFF_SQES);
        if (sq == MAP_FAILED || sqes == MAP_FAILED) { perror("mmap"); return 1; }
        unsigned *tail = (unsigned *)(sq + p.sq_off.tail), *mask = (unsigned *)(sq + p.sq_off.ring_mask);
        unsigned *array = (unsigned *)(sq + p.sq_off.array);
        unsigned t = *tail, idx = t & *mask;
        struct io_uring_sqe *s = &sqes[idx];
        memset(s, 0, sizeof *s);
        s->opcode = IORING_OP_WRITE; s->fd = fd; s->addr = (unsigned long)buf; s->len = len; s->off = 0;
        array[idx] = idx;
        __atomic_store_n(tail, t + 1, __ATOMIC_RELEASE);
        if (syscall(__NR_io_uring_enter, ring, 1, 0, 0, NULL, 0) != 1) { perror("io_uring_enter"); return 1; }
    } else {
        aio_context_t ctx = 0;
        if (syscall(__NR_io_setup, 4, &ctx)) { perror("io_setup"); return 1; }
        struct iocb cb;
        memset(&cb, 0, sizeof cb);
        cb.aio_lio_opcode = IOCB_CMD_PWRITE; cb.aio_fildes = fd; cb.aio_buf = (unsigned long)buf;
        cb.aio_nbytes = len; cb.aio_offset = 0;
        struct iocb *cbs[1] = { &cb };
        if (syscall(__NR_io_submit, ctx, 1, cbs) != 1) { perror("io_submit"); return 1; }
    }
    if (write(1, "submitted\n", 10) != 10) return 1;
    raise(SIGKILL);
    return 0;
}
