#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/filter.h>
#include <linux/io_uring.h>
#include <linux/openat2.h>
#include <linux/seccomp.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/ptrace.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <sys/xattr.h>
#include <time.h>
#include <unistd.h>

static const char *self;
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec+t.tv_nsec/1e9;}
static long putfd(long fd){if(fd<0)return fd;long n=write(fd,"AFTER!",6);int e=errno;close(fd);errno=e;return n;}
static void *threadwrite(void *p){putfd(open(p,O_WRONLY|O_CREAT|O_TRUNC,0644));return NULL;}
static void *threadexec(void *p){(void)p;execl(self,self,"child-write",NULL);_exit(97);}
static _Atomic int *control;
static char racepath[64];
static int rounds;
static void *raceworker(void *p){
    (void)p;
    for(int i=0;i<rounds;i++){
        while(atomic_load(control)!=2)sched_yield();
        strcpy(racepath,"race-evil");atomic_store(control,3);
        while(atomic_load(control)==3)sched_yield();
        atomic_store(control,5);
    }
    return NULL;
}
static _Atomic int innerfd=-1;
static void *innerlistener(void *p){
    (void)p;while(atomic_load(&innerfd)<0)sched_yield();
    struct seccomp_notif n={0};
    if(ioctl(innerfd,SECCOMP_IOCTL_NOTIF_RECV,&n))return (void*)1;
    struct seccomp_notif_resp r={.id=n.id,.flags=SECCOMP_USER_NOTIF_FLAG_CONTINUE};
    ioctl(innerfd,SECCOMP_IOCTL_NOTIF_SEND,&r);return NULL;
}
int main(int argc,char **argv){
    if(argc<2)return 2;
    self=argv[0];const char *s=argv[1];long r=-1;errno=0;
    double start=now();
    if(!strcmp(s,"child-write"))r=putfd(open("child",O_WRONLY|O_CREAT|O_TRUNC,0644));
    else if(!strcmp(s,"open"))r=putfd(syscall(SYS_open,"a",O_WRONLY,0));
    else if(!strcmp(s,"open-rdwr"))r=putfd(syscall(SYS_open,"a",O_RDWR,0));
    else if(!strcmp(s,"open-append"))r=putfd(syscall(SYS_open,"a",O_WRONLY|O_APPEND,0));
    else if(!strcmp(s,"open-trunc-readonly")){r=syscall(SYS_open,"a",O_RDONLY|O_TRUNC,0);if(r>=0)close(r);}
    else if(!strcmp(s,"open-readonly")){r=open("a",O_RDONLY);if(r>=0)close(r);}
    else if(!strcmp(s,"open-create"))r=putfd(open("new",O_WRONLY|O_CREAT|O_EXCL,0644));
    else if(!strcmp(s,"open-excl-fail"))r=putfd(open("a",O_WRONLY|O_CREAT|O_EXCL,0644));
    else if(!strcmp(s,"openat")){int d=open("sub",O_RDONLY|O_DIRECTORY);r=putfd(syscall(SYS_openat,d,"a",O_WRONLY,0));close(d);}
    else if(!strcmp(s,"cwd")){chdir("sub");r=putfd(open("a",O_WRONLY));}
    else if(!strcmp(s,"openat2")||!strcmp(s,"openat2-beneath")){
        struct open_how h={.flags=O_WRONLY,.resolve=!strcmp(s,"openat2-beneath")?RESOLVE_BENEATH:0};
        r=putfd(syscall(SYS_openat2,AT_FDCWD,"a",&h,sizeof(h)));
    }
    else if(!strcmp(s,"creat"))r=putfd(syscall(SYS_creat,"a",0644));
    else if(!strcmp(s,"tmpfile")){
        int fd=open(".",O_TMPFILE|O_RDWR,0644);if(fd>=0){write(fd,"AFTER!",6);char p[80];snprintf(p,sizeof(p),"/proc/self/fd/%d",fd);r=linkat(AT_FDCWD,p,AT_FDCWD,"new",AT_SYMLINK_FOLLOW);close(fd);}else r=-1;
    }
    else if(!strcmp(s,"tmpfile-empty")){
        int fd=open(".",O_TMPFILE|O_RDWR,0644);if(fd>=0){write(fd,"AFTER!",6);r=linkat(fd,"",AT_FDCWD,"new",AT_EMPTY_PATH);close(fd);}else r=-1;
    }
    else if(!strcmp(s,"opath")){
        int fd=open("a",O_PATH);char p[80];snprintf(p,sizeof(p),"/proc/self/fd/%d",fd);r=putfd(open(p,O_WRONLY));close(fd);
    }
    else if(!strcmp(s,"proc-cwd"))r=putfd(open("/proc/self/cwd/a",O_WRONLY));
    else if(!strcmp(s,"proc-root")){char p[4096],cwd[4000];getcwd(cwd,sizeof(cwd));snprintf(p,sizeof(p),"/proc/self/root%s/a",cwd);r=putfd(open(p,O_WRONLY));}
    else if(!strcmp(s,"symlink-alias"))r=putfd(open("alias",O_WRONLY));
    else if(!strcmp(s,"dangling-symlink"))r=putfd(open("dangling",O_WRONLY|O_CREAT,0644));
    else if(!strcmp(s,"umask")){umask(0077);r=putfd(open("new",O_WRONLY|O_CREAT,0666));}
    else if(!strcmp(s,"hardlink-alias"))r=putfd(open("target/alias",O_WRONLY));
    else if(!strcmp(s,"hardlink-normal"))r=putfd(open("hard",O_WRONLY));
    else if(!strcmp(s,"unlink"))r=syscall(SYS_unlink,"a");
    else if(!strcmp(s,"unlinkat"))r=unlinkat(AT_FDCWD,"a",0);
    else if(!strcmp(s,"rmdir"))r=rmdir("empty");
    else if(!strcmp(s,"unlinkat-dir"))r=unlinkat(AT_FDCWD,"empty",AT_REMOVEDIR);
    else if(!strcmp(s,"rename"))r=rename("a","b");
    else if(!strcmp(s,"renameat"))r=renameat(AT_FDCWD,"a",AT_FDCWD,"b");
    else if(!strcmp(s,"rename-exchange"))r=syscall(SYS_renameat2,AT_FDCWD,"a",AT_FDCWD,"b",RENAME_EXCHANGE);
    else if(!strcmp(s,"rename-noreplace"))r=syscall(SYS_renameat2,AT_FDCWD,"a",AT_FDCWD,"b",RENAME_NOREPLACE);
    else if(!strcmp(s,"rename-dir")||!strcmp(s,"rename-large"))r=rename("dir","moved");
    else if(!strcmp(s,"truncate"))r=truncate("a",2);
    else if(!strcmp(s,"link"))r=link("a","new");
    else if(!strcmp(s,"linkat"))r=linkat(AT_FDCWD,"a",AT_FDCWD,"new",0);
    else if(!strcmp(s,"symlink"))r=symlink("a","new");
    else if(!strcmp(s,"symlinkat"))r=symlinkat("a",AT_FDCWD,"new");
    else if(!strcmp(s,"unlink-symlink"))r=unlink("alias");
    else if(!strcmp(s,"mkdir"))r=mkdir("newdir",0755);
    else if(!strcmp(s,"mkdirat"))r=mkdirat(AT_FDCWD,"newdir",0755);
    else if(!strcmp(s,"chmod"))r=chmod("a",0600);
    else if(!strcmp(s,"fchmod")){int fd=open("a",O_RDONLY);r=fchmod(fd,0600);close(fd);}
    else if(!strcmp(s,"fchmodat"))r=fchmodat(AT_FDCWD,"a",0600,0);
    else if(!strcmp(s,"chown"))r=chown("a",getuid(),getgid());
    else if(!strcmp(s,"lchown"))r=lchown("alias",getuid(),getgid());
    else if(!strcmp(s,"fchown")){int fd=open("a",O_RDONLY);r=fchown(fd,getuid(),getgid());close(fd);}
    else if(!strcmp(s,"fchownat"))r=fchownat(AT_FDCWD,"a",getuid(),getgid(),0);
    else if(!strcmp(s,"utimensat")){struct timespec t[2]={{123,0},{456,0}};r=utimensat(AT_FDCWD,"a",t,0);}
    else if(!strcmp(s,"setxattr"))r=setxattr("a","user.probe","after",5,0);
    else if(!strcmp(s,"lsetxattr"))r=lsetxattr("a","user.probe","after",5,0);
    else if(!strcmp(s,"fsetxattr")){int fd=open("a",O_RDONLY);r=fsetxattr(fd,"user.probe","after",5,0);close(fd);}
    else if(!strcmp(s,"mknod"))r=mknod("fifo",S_IFIFO|0600,0);
    else if(!strcmp(s,"mknodat"))r=mknodat(AT_FDCWD,"fifo",S_IFIFO|0600,0);
    else if(strstr(s,"ftruncate")||strstr(s,"fallocate")||strstr(s,"mmap")){
        int fd=strstr(s,"inherited")?atoi(argv[2]):open("a",O_RDWR);
        if(strstr(s,"ftruncate"))r=ftruncate(fd,2);
        else if(strstr(s,"fallocate"))r=fallocate(fd,FALLOC_FL_ZERO_RANGE,0,4096);
        else {char *p=mmap(NULL,4096,PROT_READ|PROT_WRITE,MAP_SHARED,fd,0);if(p!=MAP_FAILED){memcpy(p,"AFTER!",6);r=msync(p,4096,MS_SYNC);munmap(p,4096);}else r=-1;}
        close(fd);
    }
    else if(!strcmp(s,"hidden-open"))r=putfd(open("hidden/new",O_WRONLY|O_CREAT,0644));
    else if(!strcmp(s,"hidden-rename"))r=rename("a","hidden/new");
    else if(!strcmp(s,"hidden-link"))r=link("a","hidden/new");
    else if(!strcmp(s,"hidden-symlink"))r=symlink("a","hidden/new");
    else if(!strcmp(s,"hidden-mkdir"))r=mkdir("hidden/newdir",0700);
    else if(!strcmp(s,"regenerable"))r=putfd(open("target/new",O_WRONLY|O_CREAT,0644));
    else if(!strcmp(s,"git"))r=putfd(open(".git/new",O_WRONLY|O_CREAT,0644));
    else if(!strcmp(s,"fork")||!strcmp(s,"vfork")){
        pid_t p=!strcmp(s,"vfork")?vfork():fork();if(!p){execl(self,self,"child-write",NULL);_exit(99);}int st;waitpid(p,&st,0);r=WEXITSTATUS(st);
    }
    else if(!strcmp(s,"thread")){pthread_t t;pthread_create(&t,NULL,threadwrite,"child");pthread_join(t,NULL);r=0;}
    else if(!strcmp(s,"thread-exec")){pthread_t t;pthread_create(&t,NULL,threadexec,NULL);pthread_join(t,NULL);r=99;}
    else if(!strcmp(s,"orphan")||!strcmp(s,"daemon")||!strcmp(s,"deadline")){
        pid_t p=fork();if(!p){if(strcmp(s,"orphan"))setsid();pid_t q=fork();if(q)_exit(0);usleep(100000);putfd(open("child",O_WRONLY|O_CREAT,0644));if(!strcmp(s,"deadline"))for(;;)pause();_exit(0);}int st;waitpid(p,&st,0);r=0;
    }
    else if(!strcmp(s,"ptrace-traceme")){
        pid_t p=fork();if(!p){errno=0;long v=ptrace(PTRACE_TRACEME,0,0,0);dprintf(1,"{\"ptrace_rc\":%ld,\"ptrace_errno\":%d}\n",v,v<0?errno:0);_exit(0);}int st;waitpid(p,&st,0);r=0;
    }
    else if(!strcmp(s,"ptrace-attach")){
        pid_t p=fork();if(!p){for(;;)pause();}usleep(10000);r=ptrace(PTRACE_ATTACH,p,0,0);int e=errno,st;if(!r){waitpid(p,&st,0);ptrace(PTRACE_DETACH,p,0,0);}kill(p,SIGKILL);waitpid(p,&st,0);errno=e;
    }
    else if(!strcmp(s,"filter-allow")){
        struct sock_filter b[]={BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_ALLOW)};struct sock_fprog f={1,b};
        prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0);r=prctl(PR_SET_SECCOMP,SECCOMP_MODE_FILTER,&f);
        if(!r)r=putfd(open("a",O_WRONLY));
    }
    else if(!strcmp(s,"filter-bypass")){
        pthread_t t;pthread_create(&t,NULL,innerlistener,NULL);
        struct sock_filter b[]={BPF_STMT(BPF_LD|BPF_W|BPF_ABS,offsetof(struct seccomp_data,nr)),
          BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,SYS_openat,0,1),BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_USER_NOTIF),BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_ALLOW)};
        struct sock_fprog f={4,b};prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0);
        int fd=syscall(SYS_seccomp,SECCOMP_SET_MODE_FILTER,SECCOMP_FILTER_FLAG_NEW_LISTENER,&f);
        if(fd<0){dprintf(1,"{\"install_errno\":%d}\n",errno);_exit(0);}
        atomic_store(&innerfd,fd);r=putfd(open("hidden/new",O_WRONLY|O_CREAT,0644));pthread_join(t,NULL);close(fd);
    }
    else if(!strcmp(s,"io-uring")){struct io_uring_params p={0};r=syscall(SYS_io_uring_setup,8,&p);if(r>=0)close(r);}
    else if(!strcmp(s,"nondumpable")){prctl(PR_SET_DUMPABLE,0);r=putfd(open("a",O_WRONLY));}
    else if(!strcmp(s,"micro")){
        int n=argc>2?atoi(argv[2]):20000;for(int i=0;i<n;i++){int fd=open("target/bench",O_WRONLY|O_CREAT,0644);if(fd<0){r=-1;goto done;}close(fd);}r=n;
    }
    else if(!strcmp(s,"race")){
        int fd=open(getenv("WI_RACE_CONTROL"),O_RDWR);control=mmap(NULL,4096,PROT_READ|PROT_WRITE,MAP_SHARED,fd,0);close(fd);
        struct stat good,bad;stat("race-safe",&good);stat("race-evil",&bad);
        int safe_hits=0,evil_hits=0,failed=0;
        rounds=argc>2?atoi(argv[2]):200;pthread_t t;pthread_create(&t,NULL,raceworker,NULL);
        for(int i=0;i<rounds;i++){
            strcpy(racepath,"race-safe");atomic_store(control,1);
            int f=open(racepath,O_WRONLY|O_TRUNC);if(f>=0){
                struct stat got;fstat(f,&got);safe_hits+=got.st_ino==good.st_ino;evil_hits+=got.st_ino==bad.st_ino;
                write(f,"X",1);close(f);
            }else failed++;
            atomic_store(control,4);
            /* 对端确认后才开启下一轮，避免漏过状态 4。 */
            while(atomic_load(control)!=5)sched_yield();
        }
        pthread_join(t,NULL);r=rounds;
        printf("{\"safe_hits\":%d,\"evil_hits\":%d,\"failed\":%d}\n",safe_hits,evil_hits,failed);
    }
    else if(!strcmp(s,"fs-race"))r=putfd(open("race-safe",O_WRONLY|O_TRUNC));
    else if(!strcmp(s,"hold-existing-fd")){
        int fd=atoi(argv[2]);r=putfd(open(getenv("WI_RACE_CONTROL")?"race-safe":"a",O_WRONLY));
        int err=errno;long old=write(fd,"AFTER!",6);close(fd);
        printf("{\"existing_fd_write\":%ld}\n",old);errno=err;
    }
    else if(!strcmp(s,"external-window")){usleep(150000);r=putfd(open("a",O_WRONLY));usleep(150000);}
    else if(!strcmp(s,"hold")){r=putfd(open("a",O_WRONLY));sleep(1);}
    else {fprintf(stderr,"未知探针 %s\n",s);return 2;}
done:
    printf("{\"case\":\"%s\",\"rc\":%ld,\"errno\":%d,\"seconds\":%.9f}\n",s,r,r<0?errno:0,now()-start);
    return 0;
}
