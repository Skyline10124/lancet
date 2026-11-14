#define _GNU_SOURCE
#include <errno.h>
#include <dirent.h>
#include <fcntl.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/fs.h>
#include <linux/openat2.h>
#include <linux/seccomp.h>
#include <poll.h>
#include <sched.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/prctl.h>
#include <sys/ptrace.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/user.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

/* 原型限定 x86-64；拒绝其他 ABI，避免 syscall 编号碰撞。 */
static char root[4096], state[4096];
static const char *mode;
static FILE *ledger;
static int listener = -1, statefd, journalfd;
static volatile sig_atomic_t initpid, launchpid, timedout;
static unsigned long calls, clones, denied, errors, events, emulated;
static char **paths;
static size_t npaths, cap;
static volatile int *race;
static int race_done;

static void die(const char *s) { perror(s); exit(125); }
static double now(void) {
    struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}
static void timeout_handler(int sig) {
    (void)sig; timedout = 1;
    if (initpid > 0) kill(initpid, SIGKILL);
    if (launchpid > 0) {kill(-launchpid, SIGKILL);kill(launchpid, SIGKILL);}
}
static void jsonstr(FILE *f, const char *s) {
    fputc('"', f);
    for (; *s; s++) {
        unsigned char c = *s;
        if (c == '"' || c == '\\') fprintf(f, "\\%c", c);
        else if (c < 32) fprintf(f, "\\u%04x", c);
        else fputc(c, f);
    }
    fputc('"', f);
}
static void writefile(const char *p, const char *v) {
    int fd = open(p, O_WRONLY); if (fd < 0) die(p);
    if (write(fd, v, strlen(v)) != (ssize_t)strlen(v)) die(p);
    close(fd);
}
static void sendfd(int sock, int fd) {
    char c = 'F', control[CMSG_SPACE(sizeof(int))] = {0};
    struct iovec iov = { &c, 1 };
    struct msghdr m = {.msg_iov=&iov, .msg_iovlen=1,
                      .msg_control=control, .msg_controllen=sizeof(control)};
    struct cmsghdr *h = CMSG_FIRSTHDR(&m);
    h->cmsg_level = SOL_SOCKET; h->cmsg_type = SCM_RIGHTS;
    h->cmsg_len = CMSG_LEN(sizeof(int)); memcpy(CMSG_DATA(h), &fd, sizeof(fd));
    if (sendmsg(sock, &m, 0) != 1) die("SCM_RIGHTS");
}
static int recvfd(int sock) {
    char c, control[CMSG_SPACE(sizeof(int))] = {0};
    struct iovec iov = {&c, 1};
    struct msghdr m = {.msg_iov=&iov, .msg_iovlen=1,
                      .msg_control=control, .msg_controllen=sizeof(control)};
    if (recvmsg(sock, &m, 0) != 1) return -1;
    struct cmsghdr *h = CMSG_FIRSTHDR(&m); int fd = -1;
    if (h && h->cmsg_type == SCM_RIGHTS) memcpy(&fd, CMSG_DATA(h), sizeof(fd));
    return fd;
}

static int filter(void) {
    struct sock_filter b[160]; int n = 0;
#define ST(c,k) b[n++] = (struct sock_filter)BPF_STMT(c,k)
#define JP(c,k,t,f) b[n++] = (struct sock_filter)BPF_JUMP(c,k,t,f)
    unsigned action = !strcmp(mode,"A") ? SECCOMP_RET_TRACE : SECCOMP_RET_USER_NOTIF;
    ST(BPF_LD|BPF_W|BPF_ABS, offsetof(struct seccomp_data, arch));
    JP(BPF_JMP|BPF_JEQ|BPF_K, AUDIT_ARCH_X86_64, 1, 0);
    ST(BPF_RET|BPF_K, SECCOMP_RET_KILL_PROCESS);
    ST(BPF_LD|BPF_W|BPF_ABS, offsetof(struct seccomp_data, nr));
    JP(BPF_JMP|BPF_JSET|BPF_K, 0x40000000, 0, 1);
    ST(BPF_RET|BPF_K, SECCOMP_RET_KILL_PROCESS);
    JP(BPF_JMP|BPF_JEQ|BPF_K, SYS_io_uring_setup, 0, 1);
    ST(BPF_RET|BPF_K, SECCOMP_RET_ERRNO | ENOSYS);
    int opens[] = {SYS_open, SYS_openat};
    for (int i=0;i<2;i++) {
        JP(BPF_JMP|BPF_JEQ|BPF_K, opens[i], 0, 4);
        ST(BPF_LD|BPF_W|BPF_ABS, offsetof(struct seccomp_data,args) + 8*(i+1));
        JP(BPF_JMP|BPF_JSET|BPF_K, O_WRONLY|O_RDWR|O_CREAT|O_TRUNC, 0, 1);
        ST(BPF_RET|BPF_K, action);
        ST(BPF_RET|BPF_K, SECCOMP_RET_ALLOW);
    }
    int ns[] = {SYS_openat2,SYS_creat,SYS_unlink,SYS_unlinkat,SYS_rmdir,
        SYS_rename,SYS_renameat,SYS_renameat2,SYS_truncate,
        SYS_link,SYS_linkat,SYS_symlink,SYS_symlinkat};
    for (size_t i=0;i<sizeof(ns)/sizeof(*ns);i++) {
        JP(BPF_JMP|BPF_JEQ|BPF_K, ns[i], 0, 1);
        ST(BPF_RET|BPF_K, action);
    }
    ST(BPF_RET|BPF_K, SECCOMP_RET_ALLOW);
    struct sock_fprog p = {.len=n, .filter=b};
    if (prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0)) die("no_new_privs");
    int fd = syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER,
        !strcmp(mode,"A") ? 0 : SECCOMP_FILTER_FLAG_NEW_LISTENER, &p);
    if (fd < 0) die("seccomp");
    return fd;
}

static void launch(int sock, char **argv) {
    setsid();
    if (!strcmp(mode,"none")) {
        close(sock); if (chdir(root)) die("cwd"); execvp(argv[0], argv); die("execvp");
    }
    if (!strcmp(mode,"A")) {
        if (ptrace(PTRACE_TRACEME,0,0,0)) die("TRACEME");
        raise(SIGSTOP);
    }
    uid_t uid=getuid(); gid_t gid=getgid(); char map[80];
    if (unshare(CLONE_NEWUSER)) die("user namespace");
    writefile("/proc/self/setgroups", "deny");
    snprintf(map,sizeof(map),"0 %u 1",uid); writefile("/proc/self/uid_map",map);
    snprintf(map,sizeof(map),"0 %u 1",gid); writefile("/proc/self/gid_map",map);
    if (unshare(CLONE_NEWNS|CLONE_NEWPID|CLONE_NEWNET)) die("namespaces");
    if (mount(NULL,"/",NULL,MS_REC|MS_PRIVATE,NULL)) die("private mounts");
    pid_t p=fork(); if (p<0) die("fork init");
    if (p) {
        if (send(sock,&p,sizeof(p),0)!=sizeof(p)) die("send init pid");
        close(sock); int st; while (waitpid(p,&st,0)<0 && errno==EINTR) {}
        _exit(WIFEXITED(st)?WEXITSTATUS(st):128+WTERMSIG(st));
    }
    if (mount("proc","/proc","proc",MS_NOSUID|MS_NOEXEC|MS_NODEV,NULL)) die("proc mount");
    /* PID 1 负责等待孤儿后代；只有工作进程继承过滤器。 */
    p=fork(); if (p<0) die("fork command");
    if (p) {
        close(sock); int st, result=0; pid_t w;
        while ((w=waitpid(-1,&st,0))>0 || errno==EINTR) {
            if (w==p) result=WIFEXITED(st)?WEXITSTATUS(st):128+WTERMSIG(st);
        }
        _exit(result);
    }
    if (chdir(root)) die("cwd");
    if (strcmp(mode,"ns")) {
        int fd=filter();
        if (strcmp(mode,"A")) {sendfd(sock,fd); close(fd);}
    }
    close(sock); execvp(argv[0],argv); die("execvp");
}

static int readmem(pid_t pid, uint64_t addr, void *buf, size_t n, uint64_t id) {
    char p[80]; snprintf(p,sizeof(p),"/proc/%d/mem",pid);
    int fd=open(p,O_RDONLY|O_CLOEXEC); if(fd<0) return -1;
    /* 先取得稳定的 mm 引用，再验证通知，防止 PID 复用。 */
    if (listener>=0 && ioctl(listener,SECCOMP_IOCTL_NOTIF_ID_VALID,&id)) {close(fd);return -1;}
    ssize_t r=pread(fd,buf,n,addr); close(fd); return r;
}
static int readstr(pid_t pid,uint64_t addr,char *buf,uint64_t id) {
    int n=readmem(pid,addr,buf,4095,id);
    if(n<=0) return -1;
    buf[n]=0; return memchr(buf,0,n)?0:-1;
}
static int rl(const char *p,char *out) {
    ssize_t n=readlink(p,out,4095); if(n<0)return -1; out[n]=0; return 0;
}
/* 把目标的 cwd/dirfd/proc 魔术路径投影到宿主路径；这不是完整的 VFS 解析器。 */
static int resolve(pid_t pid,int dirfd,const char *s,int follow,char *out) {
    char p[4096],base[4096],joined[8192],tmp[4096];
    if (!strncmp(s,"/proc/self/",11) || !strncmp(s,"/proc/thread-self/",18)) {
        const char *tail=s+(!strncmp(s,"/proc/self/",11)?11:18);
        snprintf(p,sizeof(p),"/proc/%d/%s",pid,tail);
        if(realpath(p,out))return 0;
        /* 不存在的末端仍需保留 Absent。 */
        snprintf(joined,sizeof(joined),"%s",p);
    } else if (s[0]=='/') {
        snprintf(p,sizeof(p),"/proc/%d/root",pid);
        if(rl(p,base))return -1;
        snprintf(joined,sizeof(joined),"%s/%s",base,s+1);
    } else {
        if(dirfd==AT_FDCWD)snprintf(p,sizeof(p),"/proc/%d/cwd",pid);
        else snprintf(p,sizeof(p),"/proc/%d/fd/%d",pid,dirfd);
        if(rl(p,base))return -1;
        snprintf(joined,sizeof(joined),"%s/%s",base,s);
    }
    if(follow && realpath(joined,out))return 0;
    snprintf(tmp,sizeof(tmp),"%s",joined);
    char *slash=strrchr(tmp,'/'); if(!slash)return -1;
    *slash=0; char leaf[4096]; snprintf(leaf,sizeof(leaf),"%s",slash+1);
    if(!realpath(tmp,base))return -1;
    snprintf(out,4096,"%s/%s",base,leaf); return 0;
}
/* 规则只设 hidden/、target/、.git/ 三个目录，匹配器不属于实验变量。 */
static int classify(const char *p) {
    size_t len=strlen(root); if(strncmp(p,root,len) || (p[len] && p[len]!='/'))return 0;
    const char *s=p+len; if(*s=='/')s++;
    if(!strcmp(s,"hidden") || !strncmp(s,"hidden/",7))return -1;
    if(!strcmp(s,"target") || !strncmp(s,"target/",7) ||
       !strcmp(s,".git") || !strncmp(s,".git/",5))return 0;
    return 1;
}
static int checkpoint(const char *p) {
    int c=classify(p); if(c<=0)return c;
    for(size_t i=0;i<npaths;i++)if(!strcmp(paths[i],p))return 0;
    struct stat st; int exists=lstat(p,&st)==0;
    if(!exists && errno!=ENOENT)return -1;
    if(exists && !S_ISREG(st.st_mode))return 0;
    char dst[4096]; snprintf(dst,sizeof(dst),"%s/%zu.clone",state,npaths);
    if(exists) {
        int src=open(p,O_RDONLY|O_CLOEXEC), dest=open(dst,O_CREAT|O_EXCL|O_WRONLY|O_CLOEXEC,0600);
        if(src<0 || dest<0 || ioctl(dest,FICLONE,src) || fsync(dest)) {
            if(src>=0)close(src);
            if(dest>=0)close(dest);
            errors++; return -1;
        }
        close(src);close(dest); clones++;
    }
    if(npaths==cap){cap=cap?cap*2:64;paths=realloc(paths,cap*sizeof(*paths));if(!paths)die("账本内存");}
    paths[npaths]=strdup(p);
    fprintf(ledger,"{\"index\":%zu,\"path\":",npaths); jsonstr(ledger,p);
    fprintf(ledger,",\"before\":\"%s\",\"mono_s\":%.9f}\n",exists?"file":"Absent",now());
    fflush(ledger); if(fsync(journalfd) || fsync(statefd))die("持久检查点");
    npaths++;return 0;
}
static int tree(const char *from,const char *to) {
    struct stat st; if(lstat(from,&st)) return checkpoint(from) || checkpoint(to);
    if(!S_ISDIR(st.st_mode))return checkpoint(from) || checkpoint(to);
    if(classify(from)==-1 || classify(to)==-1)return -1;
    if(!classify(from) && !classify(to))return 0;
    DIR *d=opendir(from); if(!d)return -1;
    struct dirent *e; int r=0;
    while((e=readdir(d))) {
        if(!strcmp(e->d_name,".")||!strcmp(e->d_name,".."))continue;
        char a[4096],b[4096];snprintf(a,sizeof(a),"%s/%s",from,e->d_name);snprintf(b,sizeof(b),"%s/%s",to,e->d_name);
        if(tree(a,b)){r=-1;break;}
    }
    closedir(d);return r;
}
struct op {
    int nr; uint64_t a[6]; char raw1[4096],raw2[4096],p[4096],q[4096];
    int open, isrename, islink, issym, flags, perm, follow;
    struct open_how how;
};
static int decode(pid_t pid,int nr,uint64_t *a,uint64_t id,struct op *o) {
    memset(o,0,sizeof(*o));o->nr=nr;memcpy(o->a,a,48);
    int i=0,j=-1,d1=AT_FDCWD,d2=AT_FDCWD;
    switch(nr) {
    case SYS_open:o->open=1;o->flags=a[1];o->perm=a[2];break;
    case SYS_creat:o->open=1;o->flags=O_CREAT|O_WRONLY|O_TRUNC;o->perm=a[1];break;
    case SYS_openat:o->open=1;d1=a[0];i=1;o->flags=a[2];o->perm=a[3];break;
    case SYS_openat2:
        o->open=1;d1=a[0];i=1;
        if(a[3]!=sizeof(o->how) || readmem(pid,a[2],&o->how,sizeof(o->how),id)!=sizeof(o->how))return -1;
        o->flags=o->how.flags;o->perm=o->how.mode;break;
    case SYS_unlinkat:d1=a[0];i=1;break;
    case SYS_rename:o->isrename=1;j=1;break;
    case SYS_renameat:case SYS_renameat2:o->isrename=1;d1=a[0];i=1;d2=a[2];j=3;break;
    case SYS_link:o->islink=1;j=1;break;
    case SYS_linkat:o->islink=1;d1=a[0];i=1;d2=a[2];j=3;break;
    case SYS_symlink:o->issym=1;j=1;break;
    case SYS_symlinkat:o->issym=1;d2=a[1];j=2;break;
    }
    if(readstr(pid,a[i],o->raw1,id))return -1;
    o->follow=(o->open && !(o->flags&O_NOFOLLOW)) || nr==SYS_truncate;
    if(nr==SYS_linkat && (!o->raw1[0] || !strncmp(o->raw1,"/proc/self/fd/",14))) {
        /* 未命名 inode 没有常规路径，只登记发布目的地；保留 proc fd 引用供代执行。 */
        if(!o->raw1[0])snprintf(o->p,sizeof(o->p),"/proc/%d/fd/%d",pid,d1);
        else snprintf(o->p,sizeof(o->p),"/proc/%d/fd/%s",pid,o->raw1+14);
    } else if(!o->issym && resolve(pid,d1,o->raw1,o->follow,o->p))return -1;
    if(j>=0) {
        if(readstr(pid,a[j],o->raw2,id) || resolve(pid,d2,o->raw2,0,o->q))return -1;
    }
    return 0;
}
static void rendezvous(struct op *o) {
    if(!race || (strstr(o->p,"/race-safe")==NULL && strstr(o->p,"/race-dir/item")==NULL))return;
    if(__atomic_load_n(race,__ATOMIC_ACQUIRE)!=1)return;
    __atomic_store_n(race,2,__ATOMIC_RELEASE);
    double end=now()+2;
    while(__atomic_load_n(race,__ATOMIC_ACQUIRE)!=3 && now()<end)sched_yield();
    race_done++;
}
static int prepare(struct op *o) {
    if(o->open && !(o->flags&(O_WRONLY|O_RDWR|O_CREAT|O_TRUNC)))return 0;
    if(classify(o->p)==-1 || (o->q[0] && classify(o->q)==-1))return -1;
    if(o->isrename) {
        if(tree(o->p,o->q))return -1;
        if(o->nr==SYS_renameat2 && (o->a[4]&RENAME_EXCHANGE) && tree(o->q,o->p))return -1;
    } else if(o->islink || o->issym) {
        if(checkpoint(o->q))return -1;
    } else if(checkpoint(o->p))return -1;
    rendezvous(o);return 0;
}
static long emulate(struct op *o) {
    if(o->open) {
        /* 保留 openat2 的 resolve 约束是产品实现的额外工作；原型显式拒绝。 */
        if(o->nr==SYS_openat2 && o->how.resolve){errno=EOPNOTSUPP;return -1;}
        return open(o->p,o->flags,o->perm);
    }
    switch(o->nr) {
    case SYS_unlink:return unlink(o->p);
    case SYS_rmdir:return rmdir(o->p);
    case SYS_unlinkat:return unlinkat(AT_FDCWD,o->p,o->a[2]);
    case SYS_rename:case SYS_renameat:return rename(o->p,o->q);
    case SYS_renameat2:return syscall(SYS_renameat2,AT_FDCWD,o->p,AT_FDCWD,o->q,o->a[4]);
    case SYS_truncate:return truncate(o->p,o->a[1]);
    case SYS_link:return link(o->p,o->q);
    case SYS_linkat:return linkat(AT_FDCWD,o->p,AT_FDCWD,o->q,
        !strncmp(o->p,"/proc/",6)?AT_SYMLINK_FOLLOW:o->a[4]);
    case SYS_symlink:case SYS_symlinkat:return symlink(o->raw1,o->q);
    default:errno=ENOSYS;return -1;
    }
}
static void notify_loop(int pidfd) {
    struct pollfd p[]={{listener,POLLIN,0},{pidfd,POLLIN,0}};
    while(!timedout) {
        int r=poll(p,2,-1); if(r<0){if(errno==EINTR)continue;die("poll");}
        if(p[1].revents)break;
        if(!(p[0].revents&POLLIN))continue;
        struct seccomp_notif n={0};
        if(ioctl(listener,SECCOMP_IOCTL_NOTIF_RECV,&n)){if(errno==ENOENT||errno==EINTR)continue;die("NOTIF_RECV");}
        calls++;
        if(getenv("WI_CLOSE_AFTER") && calls>=(unsigned)atoi(getenv("WI_CLOSE_AFTER"))) {close(listener);listener=-1;return;}
        struct op o; struct seccomp_notif_resp resp={.id=n.id};
        if(decode(n.pid,n.data.nr,(uint64_t*)n.data.args,n.id,&o) || prepare(&o)) {
            resp.error=-EACCES;denied++;
        } else if(!strcmp(mode,"B1"))resp.flags=SECCOMP_USER_NOTIF_FLAG_CONTINUE;
        else {
            if(ioctl(listener,SECCOMP_IOCTL_NOTIF_ID_VALID,&n.id))continue;
            long v=emulate(&o); int err=errno;emulated++;
            if(v<0)resp.error=-err;
            else if(o.open) {
                struct seccomp_notif_addfd ad={.id=n.id,.flags=SECCOMP_ADDFD_FLAG_SEND,
                    .srcfd=v,.newfd_flags=(o.flags&O_CLOEXEC)?O_CLOEXEC:0};
                int result=ioctl(listener,SECCOMP_IOCTL_NOTIF_ADDFD,&ad);err=errno;close(v);
                if(result>=0)continue;
                resp.error=-err;
            } else resp.val=v;
        }
        if(ioctl(listener,SECCOMP_IOCTL_NOTIF_SEND,&resp) && errno!=ENOENT && errno!=EINTR)die("NOTIF_SEND");
    }
}
static int trace_loop(pid_t first) {
    int st,result=0;pid_t p=waitpid(first,&st,0);if(p<0)die("初始 ptrace stop");
    unsigned opts=PTRACE_O_TRACESECCOMP|PTRACE_O_TRACEFORK|PTRACE_O_TRACEVFORK|
        PTRACE_O_TRACECLONE|PTRACE_O_TRACEEXEC|PTRACE_O_EXITKILL;
    if(ptrace(PTRACE_SETOPTIONS,p,0,opts))die("PTRACE_SETOPTIONS");
    ptrace(PTRACE_CONT,p,0,0);
    while((p=waitpid(-1,&st,__WALL))>0 || errno==EINTR) {
        if(p<0)continue;
        if(WIFEXITED(st)||WIFSIGNALED(st)) {
            if(p==first)result=WIFEXITED(st)?WEXITSTATUS(st):128+WTERMSIG(st);
            continue;
        }
        unsigned ev=st>>16;int sig=WSTOPSIG(st); events++;
        if(ev==PTRACE_EVENT_FORK && p==first) {
            unsigned long msg;ptrace(PTRACE_GETEVENTMSG,p,0,&msg);initpid=msg;
        }
        if(ev==PTRACE_EVENT_SECCOMP) {
            calls++;
            if(getenv("WI_CRASH_AFTER") && calls>=(unsigned)atoi(getenv("WI_CRASH_AFTER")))_exit(88);
            struct user_regs_struct r; if(ptrace(PTRACE_GETREGS,p,0,&r))die("GETREGS");
            uint64_t a[]={r.rdi,r.rsi,r.rdx,r.r10,r.r8,r.r9};struct op o;
            if(decode(p,r.orig_rax,a,0,&o)||prepare(&o)) {
                r.orig_rax=-1UL;r.rax=-EACCES;denied++;
                if(ptrace(PTRACE_SETREGS,p,0,&r))die("SETREGS");
            }
        }
        if(ev || sig==SIGSTOP || sig==SIGTRAP)sig=0;
        if(ptrace(PTRACE_CONT,p,0,sig) && errno!=ESRCH)die("PTRACE_CONT");
    }
    return result;
}
int main(int argc,char **argv) {
    if(argc<7){fprintf(stderr,"用法: intercept MODE ROOT STATE TIMEOUT -- COMMAND ...\n");return 2;}
    mode=argv[1];if(!realpath(argv[2],root)||!realpath(argv[3],state))die("实验目录");
    char p[4096];snprintf(p,sizeof(p),"%s/ledger.jsonl",state);
    ledger=fopen(p,"w");if(!ledger)die("账本");journalfd=fileno(ledger);
    fcntl(journalfd,F_SETFD,FD_CLOEXEC);statefd=open(state,O_DIRECTORY|O_CLOEXEC);
    if(getenv("WI_RACE_CONTROL")) {
        int fd=open(getenv("WI_RACE_CONTROL"),O_RDWR|O_CLOEXEC);if(fd<0)die("竞态控制");
        race=mmap(NULL,4096,PROT_READ|PROT_WRITE,MAP_SHARED,fd,0);close(fd);if(race==MAP_FAILED)die("mmap 控制");
    }
    prctl(PR_SET_CHILD_SUBREAPER,1);
    struct sigaction sa={.sa_handler=timeout_handler};sigaction(SIGALRM,&sa,NULL);
    int socks[2];if(socketpair(AF_UNIX,SOCK_SEQPACKET,0,socks))die("socketpair");
    double start=now();launchpid=fork();if(launchpid<0)die("fork");
    if(!launchpid) {close(socks[0]);fclose(ledger);close(statefd);launch(socks[1],argv+6);}
    close(socks[1]);alarm(atoi(argv[4]));int code=0,st;
    if(!strcmp(mode,"A"))code=trace_loop(launchpid);
    else {
        if(strcmp(mode,"none")) {
            pid_t init=0;if(recv(socks[0],&init,sizeof(init),0)==sizeof(init))initpid=init;
        }
        if(!strcmp(mode,"B1")||!strcmp(mode,"B2")) {
            listener=recvfd(socks[0]);if(listener<0)die("接收 listener");
            int pidfd=syscall(SYS_pidfd_open,launchpid,0);if(pidfd<0)die("pidfd_open");
            notify_loop(pidfd);close(pidfd);
        }
        while(waitpid(launchpid,&st,0)<0 && errno==EINTR){}
        code=WIFEXITED(st)?WEXITSTATUS(st):128+WTERMSIG(st);
        while(waitpid(-1,&st,0)>0 || errno==EINTR){}
    }
    alarm(0);close(socks[0]);if(listener>=0)close(listener);fclose(ledger);
    snprintf(p,sizeof(p),"%s/stats.json",state);FILE *f=fopen(p,"w");
    fprintf(f,"{\"mode\":\"%s\",\"wall_s\":%.9f,\"code\":%d,\"calls\":%lu,\"registered\":%zu,\"clones\":%lu,\"denied\":%lu,\"errors\":%lu,\"events\":%lu,\"emulated\":%lu,\"timeout\":%d,\"race_handshakes\":%d}\n",mode,now()-start,code,calls,npaths,clones,denied,errors,events,emulated,timedout,race_done);
    fclose(f);return timedout?124:code;
}
