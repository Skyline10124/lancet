#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/capability.h>
#include <linux/mount.h>
#include <limits.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static char state[PATH_MAX], root[PATH_MAX];
static void fail(const char *s){perror(s);exit(125);}
static void put(const char *p,const char *s){int f=open(p,O_WRONLY);if(f<0||write(f,s,strlen(s))!=(ssize_t)strlen(s))fail(p);close(f);}
static void one_dir(const char *p){struct stat st;if(mkdir(p,0755)&&errno!=EEXIST)fail(p);if(lstat(p,&st)||!S_ISDIR(st.st_mode)){errno=ENOTDIR;fail("挂载目标包含非目录或符号链接");}}
static void dirs(const char *p){char b[PATH_MAX];snprintf(b,sizeof b,"%s",p);for(char *q=b+1;*q;q++)if(*q=='/'){*q=0;one_dir(b);*q='/';}one_dir(b);}
static void join(char *out,const char *a,const char *b){if(snprintf(out,PATH_MAX,"%s%s",a,b)>=PATH_MAX){errno=ENAMETOOLONG;fail("路径");}}
static void bind_path(const char *src,const char *dst,int writable){
    char canonical[PATH_MAX],dest[PATH_MAX];struct stat st;
    if(!realpath(src,canonical)||stat(canonical,&st))fail(src);
    if(writable && (strncmp(canonical,state,strlen(state))||canonical[strlen(state)]!='/')){errno=EPERM;fail("可写挂载来源不在状态目录");}
    join(dest,root,dst);
    if(S_ISDIR(st.st_mode))dirs(dest);else{char par[PATH_MAX];snprintf(par,sizeof par,"%s",dest);*strrchr(par,'/')=0;dirs(par);int f=open(dest,O_CREAT|O_WRONLY|O_NOFOLLOW,0600);if(f<0)fail(dest);close(f);}
    if(mount(canonical,dest,NULL,MS_BIND,NULL))fail("bind");
    if(!writable && mount(NULL,dest,NULL,MS_BIND|MS_REMOUNT|MS_RDONLY|MS_NOSUID|MS_NODEV,NULL))fail("只读 bind");
}
static void private_dir(const char *name,const char *dst){char src[PATH_MAX];join(src,state,name);dirs(src);bind_path(src,dst,1);}
static void death(pid_t p){if(prctl(PR_SET_PDEATHSIG,SIGKILL)||getppid()!=p)_exit(125);}
int main(int argc,char **argv){
    if(argc<5||strcmp(argv[3],"--")){fprintf(stderr,"用法: namespace-v7 ROOT STATE -- COMMAND\n");return 2;}
    death(getppid());if(!realpath(argv[2],state))fail("状态目录");join(root,state,"/root");dirs(root);
    const char *home=getenv("S3_HOST_HOME");if(!home)home=getenv("HOME");if(!home||*home!='/')return 125;
    uid_t uid=getuid();gid_t gid=getgid();char map[80],src[PATH_MAX],dst[PATH_MAX];
    if(unshare(CLONE_NEWUSER))fail("用户命名空间");
    put("/proc/self/setgroups","deny");
    snprintf(map,sizeof map,"0 %u 1",uid);put("/proc/self/uid_map",map);snprintf(map,sizeof map,"0 %u 1",gid);put("/proc/self/gid_map",map);
    if(unshare(CLONE_NEWNS|CLONE_NEWPID|CLONE_NEWNET))fail("命名空间");
    if(mount(NULL,"/",NULL,MS_REC|MS_PRIVATE,NULL))fail("私有挂载");
    if(mount(root,root,NULL,MS_BIND,NULL))fail("根挂载");
    const char *system[]={"/usr","/etc",NULL};for(int i=0;system[i];i++)bind_path(system[i],system[i],0);
    const char *links[]={"bin","sbin","lib","lib64",NULL};for(int i=0;links[i];i++){char target[80];char suffix[80];snprintf(suffix,sizeof suffix,"/%s",links[i]);join(dst,root,suffix);snprintf(target,sizeof target,"usr/%s",links[i]);if(symlink(target,dst)&&errno!=EEXIST)fail("系统链接");}
    private_dir("/home",home);setenv("HOME",home,1);
    const char *tool=getenv("S3_TOOLCHAIN");if(tool&&*tool)bind_path(tool,tool,0);
    const char *cargo_bin=getenv("S3_CARGO_BIN");if(cargo_bin&&*cargo_bin)bind_path(cargo_bin,cargo_bin,0);
    const char *read=getenv("S3_READONLY");if(read&&*read){char *list=strdup(read),*save=NULL;for(char *q=strtok_r(list,":",&save);q;q=strtok_r(NULL,":",&save))bind_path(q,q,0);free(list);}
    // 使用者显式列出的只读工作目录；已有 hidden 用空目录遮蔽。
    const char *ro=getenv("S3_RO_WORK");if(ro&&*ro){bind_path(ro,ro,0);snprintf(src,sizeof src,"%s/hidden",ro);if(access(src,F_OK)==0){join(src,state,"/mask");dirs(src);snprintf(dst,sizeof dst,"%s/hidden",ro);bind_path(src,dst,0);}}
    join(src,state,"/shadow");bind_path(src,argv[1],1);
    for(int i=0;i<2;i++){const char *part=i?"/.git":"/target";char suffix[80];snprintf(suffix,sizeof suffix,"/shadow%s",part);join(src,state,suffix);snprintf(dst,sizeof dst,"%s%s",argv[1],part);bind_path(src,dst,1);}
    snprintf(src,sizeof src,"%s/.git/objects",argv[1]);
    if(access(src,F_OK)==0){snprintf(dst,sizeof dst,"%s/.git/host-objects-v7",argv[1]);bind_path(src,dst,0);}
    private_dir("/tmp","/tmp");
    // 设备只开放最小字符设备，目录本身来自状态；不暴露宿主 /dev 树。
    private_dir("/dev","/dev");const char *dev[]={"null","zero","random","urandom",NULL};
    for(int i=0;dev[i];i++){snprintf(src,sizeof src,"/dev/%s",dev[i]);join(dst,root,src);int f=open(dst,O_CREAT|O_WRONLY|O_NOFOLLOW,0666);if(f<0)fail(dst);close(f);if(mount(src,dst,NULL,MS_BIND,NULL))fail("设备");}
    join(dst,root,"/proc");dirs(dst);
    if(mount(NULL,root,NULL,MS_BIND|MS_REMOUNT|MS_RDONLY|MS_NOSUID|MS_NODEV,NULL))fail("只读根");
    pid_t init=fork();if(init<0)fail("fork init");if(init){int st;while(waitpid(init,&st,0)<0&&errno==EINTR){}return WIFEXITED(st)?WEXITSTATUS(st):128+WTERMSIG(st);}
    if(prctl(PR_SET_PDEATHSIG,SIGKILL))fail("失效联动");
    join(dst,root,"/proc");if(mount("proc",dst,"proc",MS_NOSUID|MS_NOEXEC|MS_NODEV,NULL))fail("proc");
    if(chroot(root)||chdir(argv[1]))fail("切换根目录");
    pid_t child=fork();if(child<0)fail("fork command");if(child){int st;while(waitpid(child,&st,0)<0&&errno==EINTR){}return WIFEXITED(st)?WEXITSTATUS(st):128+WTERMSIG(st);}
    struct __user_cap_header_struct h={.version=_LINUX_CAPABILITY_VERSION_3,.pid=0};struct __user_cap_data_struct d[2]={{0},{0}};
    if(syscall(SYS_capset,&h,&d)||prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0))fail("去除能力");
    syscall(SYS_close_range,3,~0U,0);execvp(argv[4],argv+4);fail("执行");
}
