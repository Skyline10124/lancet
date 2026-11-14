#define main baseline_probe_main
#include "probe.c"
#undef main
#include <linux/capability.h>
#include <sys/mount.h>
#ifndef SYS_fchmodat2
#define SYS_fchmodat2 452
#endif
static _Atomic int *shared(void){int fd=open(getenv("WI_RACE_CONTROL"),O_RDWR);if(fd<0)exit(90);void *p=mmap(NULL,4096,PROT_READ|PROT_WRITE,MAP_SHARED,fd,0);close(fd);return p;}
static void nocap(void){struct __user_cap_header_struct h={_LINUX_CAPABILITY_VERSION_3,0};struct __user_cap_data_struct c[2]={0};syscall(SYS_capget,&h,c);c[0].effective&=~((1U<<CAP_DAC_OVERRIDE)|(1U<<CAP_DAC_READ_SEARCH));syscall(SYS_capset,&h,c);}
static int v2_probe_main(int argc,char **argv){
    if(argc<2)return 2;
    const char *s=argv[1];long r=-1;errno=0;double start=now();
    if(!strcmp(s,"symlink-proc-text"))r=symlink("/proc/self/cwd/a","new");
    else if(!strcmp(s,"mkdir-trailing"))r=mkdir("newdir/",0755);
    else if(!strcmp(s,"open-idle")){r=open("a",O_WRONLY);if(r>=0)close(r);}
    else if(!strcmp(s,"nofollow"))r=putfd(open("alias",O_WRONLY|O_NOFOLLOW));
    else if(!strcmp(s,"excl-symlink"))r=putfd(open("alias",O_WRONLY|O_CREAT|O_EXCL,0666));
    else if(!strcmp(s,"cloexec")){int fd=open("a",O_WRONLY|O_CLOEXEC);r=fd<0?-1:fcntl(fd,F_GETFD);if(fd>=0)close(fd);}
    else if(!strcmp(s,"not-cloexec")){int fd=open("a",O_WRONLY);r=fd<0?-1:fcntl(fd,F_GETFD);if(fd>=0)close(fd);}
    else if(!strcmp(s,"fchmodat2"))r=syscall(SYS_fchmodat2,AT_FDCWD,"a",0601,0);
    else if(!strcmp(s,"removexattr"))r=removexattr("a","user.probe");
    else if(!strcmp(s,"lremovexattr"))r=lremovexattr("a","user.probe");
    else if(!strcmp(s,"fremovexattr")){int fd=open("a",O_RDONLY);r=fremovexattr(fd,"user.probe");close(fd);}
    else if(!strcmp(s,"cross-target"))r=link("a","target/new");
    else if(!strcmp(s,"cross-git"))r=link("a",".git/new");
    else if(!strcmp(s,"cross-target-git"))r=link("target/a",".git/new");
    else if(!strcmp(s,"within-target"))r=link("target/a","target/new");
    else if(!strcmp(s,"unlink-hardlink"))r=unlink("hard");
    else if(!strcmp(s,"rename-hardlink"))r=rename("hard","new");
    else if(!strcmp(s,"git-hardlink"))r=putfd(open(".git/alias",O_WRONLY));
    else if(!strncmp(s,"hidden-",7)){
        if(!strcmp(s,"hidden-creat"))r=putfd(creat("hidden/new",0600));
        else if(!strcmp(s,"hidden-openat"))r=putfd(openat(AT_FDCWD,"hidden/new",O_CREAT|O_WRONLY,0600));
        else if(!strcmp(s,"hidden-openat2")){struct open_how h={.flags=O_CREAT|O_WRONLY,.mode=0600};r=putfd(syscall(SYS_openat2,AT_FDCWD,"hidden/new",&h,sizeof(h)));}
        else if(!strcmp(s,"hidden-mkdirat"))r=mkdirat(AT_FDCWD,"hidden/new",0700);
        else if(!strcmp(s,"hidden-mknod"))r=mknod("hidden/new",S_IFIFO|0600,0);
        else if(!strcmp(s,"hidden-mknodat"))r=mknodat(AT_FDCWD,"hidden/new",S_IFIFO|0600,0);
        else if(!strcmp(s,"hidden-symlinkat"))r=symlinkat("a",AT_FDCWD,"hidden/new");
        else if(!strcmp(s,"hidden-linkat"))r=linkat(AT_FDCWD,"a",AT_FDCWD,"hidden/new",0);
        else if(!strcmp(s,"hidden-renameat"))r=renameat(AT_FDCWD,"a",AT_FDCWD,"hidden/new");
        else if(!strcmp(s,"hidden-renameat2"))r=syscall(SYS_renameat2,AT_FDCWD,"a",AT_FDCWD,"hidden/new",0);
        else return baseline_probe_main(argc,argv);
    }
    else if(!strncmp(s,"resolve-",8)) {
        int d=open("sub",O_PATH|O_DIRECTORY);const char *path="a";struct open_how h={.flags=O_WRONLY};
        if(!strcmp(s,"resolve-beneath"))h.resolve=RESOLVE_BENEATH;
        if(!strcmp(s,"resolve-beneath-escape")){h.resolve=RESOLVE_BENEATH;path="../a";}
        if(!strcmp(s,"resolve-in-root")){h.resolve=RESOLVE_IN_ROOT;path="/a";}
        if(!strcmp(s,"resolve-in-root-create")){h.resolve=RESOLVE_IN_ROOT;path="/new";h.flags|=O_CREAT;h.mode=0666;}
        if(!strcmp(s,"resolve-nosym")){h.resolve=RESOLVE_NO_SYMLINKS;path="../alias";}
        if(!strcmp(s,"resolve-nomagic")){h.resolve=RESOLVE_NO_MAGICLINKS;path="/proc/self/cwd/a";}
        if(!strcmp(s,"resolve-noxdev")){h.resolve=RESOLVE_NO_XDEV;path="/dev/null";}
        if(!strcmp(s,"resolve-noxdev-ok"))h.resolve=RESOLVE_NO_XDEV;
        if(!strcmp(s,"resolve-nosym-ok"))h.resolve=RESOLVE_NO_SYMLINKS;
        if(!strcmp(s,"resolve-nomagic-ok"))h.resolve=RESOLVE_NO_MAGICLINKS;
        if(!strcmp(s,"resolve-dangling-nosym")){path="../dangling";h.resolve=RESOLVE_NO_SYMLINKS;h.flags|=O_CREAT;h.mode=0600;}
        r=putfd(syscall(SYS_openat2,d,path,&h,sizeof(h)));close(d);
    }
    else if(!strncmp(s,"perm-",5)) {
        if(strstr(s,"nocap"))nocap();
        r=putfd(open(strstr(s,"dir")?"readonly/new":"readonly-file",O_WRONLY|(strstr(s,"dir")?O_CREAT:0),0666));
    }
    else if(!strncmp(s,"tmpfs-perm",10)) {
        if(mount("tmpfs","tmpmount","tmpfs",0,"size=1m")){r=-1;goto done_v2;}
        int fd=open("tmpmount/file",O_CREAT|O_WRONLY,0600);if(fd<0)goto done_v2;close(fd);chmod("tmpmount/file",0400);
        if(strstr(s,"nocap"))nocap();
        r=putfd(open("tmpmount/file",O_WRONLY));
        /* 不卸载；命名空间退出时回收。该文件在节点 mount namespace 不可见。 */
    }
    else if(!strcmp(s,"chroot")){if(chroot("jail")||chdir("/"))goto done_v2;r=putfd(open("/a",O_WRONLY));}
    else if(!strncmp(s,"replace-",8)) {
        if(!strcmp(s,"replace-open-inside")) {
            _Atomic int *c=shared();pid_t p=fork();
            if(!p){while(atomic_load(c)!=2)sched_yield();rename("race-safe","spare");symlink("race-evil","race-safe");atomic_store(c,3);_exit(0);}
            atomic_store(c,1);r=putfd(open("race-safe",O_WRONLY|O_TRUNC));int st;waitpid(p,&st,0);
        }else if(!strcmp(s,"replace-open"))r=putfd(open("race-safe",O_WRONLY|O_TRUNC));
        else if(!strcmp(s,"replace-symlink"))r=symlink("a","race-safe");
        else if(!strcmp(s,"replace-symlinkat"))r=symlinkat("a",AT_FDCWD,"race-safe");
        else if(!strcmp(s,"replace-rename"))r=rename("race-safe","dest");
        else if(!strcmp(s,"replace-renameat"))r=renameat(AT_FDCWD,"race-safe",AT_FDCWD,"dest");
        else if(!strcmp(s,"replace-renameat2"))r=syscall(SYS_renameat2,AT_FDCWD,"race-safe",AT_FDCWD,"dest",0);
        else if(!strcmp(s,"replace-unlink"))r=unlink("race-safe");
        else if(!strcmp(s,"replace-unlinkat"))r=unlinkat(AT_FDCWD,"race-safe",0);
        else if(!strcmp(s,"replace-rmdir"))r=rmdir("race-safe");
        else if(!strcmp(s,"replace-link"))r=link("race-safe","dest");
        else if(!strcmp(s,"replace-linkat"))r=linkat(AT_FDCWD,"race-safe",AT_FDCWD,"dest",0);
    }
    else if(!strcmp(s,"crash-old")) {
        int fd=open("b",O_RDWR);if(fd<0)goto done_v2;
        r=open("race-safe",O_WRONLY);int e=errno;
        long old=write(fd,"AFTER!",6);printf("{\"existing_fd_write\":%ld,\"open_errno\":%d}\n",old,r<0?e:0);
        if(r>=0)close(r);
        close(fd);errno=e;
    }
    else if(!strcmp(s,"cancel-open")) {
        _Atomic int *c=shared();pid_t p=fork();
        if(!p){r=open("race-safe",O_WRONLY|O_TRUNC);_exit(r>=0?0:2);}
        while(atomic_load(c)!=2)sched_yield();
        kill(p,SIGKILL);int st;waitpid(p,&st,0);atomic_store(c,3);r=0;
    }
    else if(!strcmp(s,"cancel-unlink")) {
        _Atomic int *c=shared();pid_t p=fork();if(!p){unlink("race-safe");_exit(0);}
        while(atomic_load(c)!=2)sched_yield();
        kill(p,SIGKILL);int st;waitpid(p,&st,0);atomic_store(c,3);r=0;
    }
    else return baseline_probe_main(argc,argv);
done_v2:
    printf("{\"case\":\"%s\",\"rc\":%ld,\"errno\":%d,\"seconds\":%.9f}\n",s,r,r<0?errno:0,now()-start);return 0;
}

static char mutable_a[4096],mutable_b[4096];
static _Atomic int *v3_control;
static const char *swap_a,*swap_b;
static void *swap_thread(void *unused) {
    (void)unused;
    while(atomic_load(v3_control)!=2)sched_yield();
    if(swap_a)strcpy(mutable_a,swap_a);
    if(swap_b)strcpy(mutable_b,swap_b);
    atomic_store(v3_control,3);return NULL;
}
static long op(const char *kind,const char *a,const char *b) {
    if(!strcmp(kind,"open")){int fd=open(a,O_WRONLY|O_TRUNC);if(fd>=0){write(fd,"RACE",4);close(fd);}return fd;}
    if(!strcmp(kind,"truncate"))return truncate(a,0);
    if(!strcmp(kind,"rename"))return rename(a,b);
    if(!strcmp(kind,"unlink"))return unlink(a);
    if(!strcmp(kind,"link"))return link(a,b);
    if(!strcmp(kind,"symlink"))return symlink("a",b);
    if(!strcmp(kind,"mkdir"))return mkdir(a,0700);
    return -2;
}
int main(int argc,char **argv) {
    if(argc<2)return 2;
    const char *s=argv[1];
    if(!strcmp(s,"crash-v3")) {
        _Atomic int *c=shared();int idle=open("a",O_WRONLY);if(idle>=0)close(idle);
        int fd=open("b",O_WRONLY),allowed=open("target/a",O_WRONLY);if(fd<0||allowed<0)return 2;
        int r=open("race-safe",O_WRONLY);int e=errno;
        atomic_store(c+4,r<0?e:0);long wrote=write(fd,"AFTER!",6);atomic_store(c+5,wrote);
        atomic_store(c+6,write(allowed,"AFTER!",6));close(allowed);
        if(r>=0)close(r);
        close(fd);return 0;
    }
    if(!strncmp(s,"replace-",8)) {putfd(open("b",O_WRONLY|O_TRUNC));return v2_probe_main(argc,argv);}
    if(!strcmp(s,"invalid-dirfd")) {
        syscall(SYS_close_range,3,~0U,0);int wrong=0;
        for(int i=0;i<1000;i++) {
            errno=0;int bad=openat(3+i%28,"a",O_WRONLY);if(bad>=0||errno!=EBADF)wrong++;if(bad>=0)close(bad);
            int good=open("a",O_WRONLY);if(good<0)wrong++;else close(good);
        }
        printf("{\"iterations\":1000,\"wrong\":%d}\n",wrong);return wrong?1:0;
    }
    if(!strcmp(s,"ll-race")) {
        const char *kind=argv[2];int total=atoi(argv[3]),ok=0,denied_count=0,other=0;v3_control=shared();
        for(int i=0;i<total;i++) {
            strcpy(mutable_a,"target/race-safe");strcpy(mutable_b,"target/dest");swap_a="race-evil";swap_b=NULL;
            if(!strcmp(kind,"link")||!strcmp(kind,"rename")){swap_a=NULL;swap_b="race-evil-dest";}
            if(!strcmp(kind,"symlink")){strcpy(mutable_b,"target/race-safe-new");swap_a=NULL;swap_b="race-evil-dest";}
            if(!strcmp(kind,"mkdir")){strcpy(mutable_a,"target/race-safe-new");swap_a="race-evil-new";}
            if(argc>4&&!strcmp(argv[4],"hardlink")){swap_a="target/preexisting-alias";}
            if(argc>4&&!strcmp(argv[4],"writable-cross")){swap_a=NULL;swap_b=".git/cross-new";}
            atomic_store(v3_control,1);pthread_t t;pthread_create(&t,NULL,swap_thread,NULL);
            errno=0;long r=op(kind,mutable_a,mutable_b);int e=errno;pthread_join(t,NULL);
            if(r>=0)ok++;else if(e==EACCES||e==EXDEV)denied_count++;else other++;
        }
        printf("{\"case\":\"ll-race\",\"kind\":\"%s\",\"iterations\":%d,\"succeeded\":%d,\"denied\":%d,\"other\":%d}\n",kind,total,ok,denied_count,other);return 0;
    }
    if(!strcmp(s,"directory-lock")) {
        v3_control=shared();pid_t child=fork();
        if(!child){while(atomic_load(v3_control)!=2)sched_yield();atomic_store(v3_control+1,1);int fd=open("sub/a",O_WRONLY);atomic_store(v3_control+2,1);if(fd>=0)close(fd);_exit(0);}
        atomic_store(v3_control,1);int r=rename("race-safe-dir","moved");atomic_store(v3_control,3);int st;waitpid(child,&st,0);
        printf("{\"rc\":%d,\"inside_lock\":%d,\"attempted\":%d}\n",r,atomic_load(v3_control+3),atomic_load(v3_control+1));return r<0;
    }
    if(!strcmp(s,"inside-race")) {
        const char *pattern=argv[2];v3_control=shared();pid_t p=fork();
        if(!p) {
            while(atomic_load(v3_control)!=2)sched_yield();
            atomic_store(v3_control+1,1);
            long r;
            if(!strcmp(pattern,"remove")){r=unlink("race-safe");link("replacement","race-safe");}
            else if(!strcmp(pattern,"move")){r=rename("race-safe","spare");link("replacement","race-safe");}
            else if(!strcmp(pattern,"target")){r=unlink("dest");rename("replacement","dest");}
            else {r=rename("race-safe","spare");symlink("a","race-safe");}
            int e=errno;atomic_store(v3_control+2,1);
            printf("{\"attacker_rc\":%ld,\"attacker_errno\":%d}\n",r,r<0?e:0);fflush(stdout);_exit(0);
        }
        atomic_store(v3_control,1);
        long r=(!strcmp(pattern,"aba"))?symlink("a","race-safe"):
             (!strcmp(pattern,"target"))?rename("race-safe","dest"):unlink("race-safe");
        int e=errno;int during=atomic_load(v3_control+2),attempt=atomic_load(v3_control+1);
        atomic_store(v3_control,3);int st;waitpid(p,&st,0);
        printf("{\"victim_rc\":%ld,\"victim_errno\":%d,\"attacker_attempted\":%d,\"attacker_finished_at_return\":%d}\n",r,r<0?e:0,attempt,during);return 0;
    }
    if(!strcmp(s,"micro-path")) {
        const char *path=argv[2];int n=atoi(argv[3]),fails=0;double start=now();
        for(int i=0;i<n;i++){int fd=open(path,O_WRONLY);if(fd<0)fails++;else close(fd);}
        double elapsed=now()-start;printf("{\"case\":\"micro-path\",\"iterations\":%d,\"failed\":%d,\"seconds\":%.9f,\"us_per_call\":%.6f}\n",n,fails,elapsed,elapsed*1e6/n);return fails?1:0;
    }
    if(!strcmp(s,"rename-exchange-dir")){double start=now();int r=syscall(SYS_renameat2,AT_FDCWD,"dir",AT_FDCWD,"empty",RENAME_EXCHANGE);printf("{\"rc\":%d,\"seconds\":%.9f}\n",r,now()-start);return r<0;}
    if(!strcmp(s,"rename-edit")){double start=now();int r=rename("dir","moved");if(!r)r=putfd(open("moved/f0000",O_WRONLY|O_TRUNC));printf("{\"rc\":%d,\"seconds\":%.9f}\n",r,now()-start);return r<0;}
    if(!strcmp(s,"rename-cross")){double start=now();int r=rename("dir","target/moved");printf("{\"rc\":%d,\"errno\":%d,\"seconds\":%.9f}\n",r,r<0?errno:0,now()-start);return r<0;}
    if(!strcmp(s,"anchor-unlink-all")){unlink("hard");unlink("a");return 0;}
    if(!strcmp(s,"anchor-overwrite")){return rename("b","hard");}
    if(!strcmp(s,"parallel-prestates")) {
        int n=atoi(argv[2]);for(int i=0;i<n;i++){pid_t p=fork();if(!p){char path[80];snprintf(path,sizeof(path),"d%d/a",i);int r=putfd(open(path,O_WRONLY|O_TRUNC));_exit(r<0);}}
        int st,failed=0;while(wait(&st)>0)if(st)failed++;printf("{\"children\":%d,\"failed\":%d}\n",n,failed);return failed?1:0;
    }
    if(!strcmp(s,"worker-visibility")) {
        char host[64];snprintf(host,sizeof(host),"/proc/%s",argv[2]);printf("{\"host_worker_visible\":%d}\n",access(host,F_OK)==0);return 0;
    }
    return v2_probe_main(argc,argv);
}
