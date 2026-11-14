#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/capability.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static void fail(const char *s) {perror(s);exit(125);}
static void put(const char *p,const char *s) {
    int f=open(p,O_WRONLY);if(f<0||write(f,s,strlen(s))!=(ssize_t)strlen(s))fail(p);close(f);
}
static void parent_death(pid_t p) {
    if(prctl(PR_SET_PDEATHSIG,SIGKILL)||getppid()!=p)_exit(125);
}
int main(int argc,char **argv) {
    if(argc<5||strcmp(argv[3],"--")){fprintf(stderr,"用法: namespace-v4 ROOT STATE -- COMMAND\n");return 2;}
    pid_t parent=getppid();parent_death(parent);
    uid_t uid=getuid();gid_t gid=getgid();char p[8192],map[80];
    int sources[2];const char *names[]={"target",".git"};
    for(int i=0;i<2;i++){snprintf(p,sizeof(p),"%s/%s",argv[1],names[i]);sources[i]=open(p,O_PATH|O_DIRECTORY|O_CLOEXEC);}
    if(unshare(CLONE_NEWUSER))fail("用户命名空间");
    put("/proc/self/setgroups","deny");
    snprintf(map,sizeof(map),"0 %u 1",uid);put("/proc/self/uid_map",map);
    snprintf(map,sizeof(map),"0 %u 1",gid);put("/proc/self/gid_map",map);
    if(unshare(CLONE_NEWNS|CLONE_NEWPID|CLONE_NEWNET))fail("命名空间");
    if(mount(NULL,"/",NULL,MS_REC|MS_PRIVATE,NULL))fail("私有挂载");
    /* 先建立可见的中转挂载；被覆盖的旧 mount fd 不能直接作 bind 源。 */
    for(int i=0;i<2;i++){
        if(sources[i]<0)continue;
        char src[8192];snprintf(src,sizeof(src),"%s/%s",argv[1],names[i]);
        snprintf(p,sizeof(p),"%s/pass-%d",argv[2],i);
        if(mount(src,p,NULL,MS_BIND,NULL))fail("中转挂载");
    }
    snprintf(p,sizeof(p),"%s/shadow",argv[2]);
    if(mount(p,argv[1],NULL,MS_BIND,NULL))fail("影子挂载");
    for(int i=0;i<2;i++){
        if(sources[i]<0)continue;
        char src[8192];snprintf(src,sizeof(src),"%s/pass-%d",argv[2],i);
        snprintf(p,sizeof(p),"%s/%s",argv[1],names[i]);
        if(mount(src,p,NULL,MS_BIND,NULL))fail("类别挂载");
        close(sources[i]);
    }
    snprintf(p,sizeof(p),"%s/hidden",argv[1]);
    if(mount("tmpfs",p,"tmpfs",MS_RDONLY|MS_NOSUID|MS_NODEV,"size=4096,mode=000"))fail("hidden 遮蔽");
    /* 挂载引用已持有影子；遮蔽状态目录避免子进程直接改账本。 */
    if(mount("tmpfs",argv[2],"tmpfs",MS_RDONLY|MS_NOSUID|MS_NODEV,"size=4096,mode=000"))fail("状态遮蔽");
    pid_t init=fork();if(init<0)fail("fork init");
    if(init){int st;while(waitpid(init,&st,0)<0&&errno==EINTR){}return WIFEXITED(st)?WEXITSTATUS(st):128+WTERMSIG(st);}
    if(prctl(PR_SET_PDEATHSIG,SIGKILL))fail("init 失效联动");
    if(mount("proc","/proc","proc",MS_NOSUID|MS_NOEXEC|MS_NODEV,NULL))fail("proc");
    pid_t child=fork();if(child<0)fail("fork command");
    if(child){int st;while(waitpid(child,&st,0)<0&&errno==EINTR){}return WIFEXITED(st)?WEXITSTATUS(st):128+WTERMSIG(st);}
    /* 防止再次卸载类别边界；普通文件访问保持拥有者权限。 */
    struct __user_cap_header_struct h={.version=_LINUX_CAPABILITY_VERSION_3,.pid=0};
    struct __user_cap_data_struct d[2]={{0},{0}};
    if(syscall(SYS_capset,&h,&d)||prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0))fail("去除能力");
    syscall(SYS_close_range,3,~0U,0);
    if(chdir(argv[1]))fail("工作目录");
    execvp(argv[4],argv+4);fail("执行");
}
