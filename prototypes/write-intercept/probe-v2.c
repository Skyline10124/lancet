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
int main(int argc,char **argv){
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
