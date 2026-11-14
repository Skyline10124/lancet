/* 第二轮复用第一轮的 ABI 过滤器、启动基础与读参数函数；B2 逻辑在本文件替换。 */
#define main baseline_main
#include "intercept.c"
#undef main
#include <linux/capability.h>
#ifndef SYS_fchmodat2
#define SYS_fchmodat2 452
#endif

static int uncertain, reply_failures, checks, mismatches, worker_count;
static unsigned long link_calls,link_success;
static int v2_filter(void) {
    struct sock_filter b[256]; int n=0;
    ST(BPF_LD|BPF_W|BPF_ABS, offsetof(struct seccomp_data,arch));
    JP(BPF_JMP|BPF_JEQ|BPF_K,AUDIT_ARCH_X86_64,1,0);
    ST(BPF_RET|BPF_K,SECCOMP_RET_KILL_PROCESS);
    ST(BPF_LD|BPF_W|BPF_ABS,offsetof(struct seccomp_data,nr));
    JP(BPF_JMP|BPF_JSET|BPF_K,0x40000000,0,1);
    ST(BPF_RET|BPF_K,SECCOMP_RET_KILL_PROCESS);
    JP(BPF_JMP|BPF_JEQ|BPF_K,SYS_io_uring_setup,0,1);
    ST(BPF_RET|BPF_K,SECCOMP_RET_ERRNO|ENOSYS);
    int x[]={SYS_setxattr,SYS_lsetxattr,SYS_fsetxattr,SYS_removexattr,SYS_lremovexattr,SYS_fremovexattr};
    for(size_t i=0;i<sizeof(x)/sizeof(*x);i++) {
        JP(BPF_JMP|BPF_JEQ|BPF_K,x[i],0,1);
        ST(BPF_RET|BPF_K,SECCOMP_RET_ERRNO|ENOTSUP);
    }
    int opens[]={SYS_open,SYS_openat};
    for(int i=0;i<2;i++) {
        JP(BPF_JMP|BPF_JEQ|BPF_K,opens[i],0,4);
        ST(BPF_LD|BPF_W|BPF_ABS,offsetof(struct seccomp_data,args)+8*(i+1));
        JP(BPF_JMP|BPF_JSET|BPF_K,O_WRONLY|O_RDWR|O_CREAT|O_TRUNC,0,1);
        ST(BPF_RET|BPF_K,SECCOMP_RET_USER_NOTIF);
        ST(BPF_RET|BPF_K,SECCOMP_RET_ALLOW);
    }
    int ns[]={SYS_openat2,SYS_creat,SYS_unlink,SYS_unlinkat,SYS_rmdir,SYS_rename,
      SYS_renameat,SYS_renameat2,SYS_truncate,SYS_link,SYS_linkat,SYS_symlink,SYS_symlinkat,
      SYS_mkdir,SYS_mkdirat,SYS_mknod,SYS_mknodat,SYS_chmod,SYS_fchmod,SYS_fchmodat,SYS_fchmodat2};
    for(size_t i=0;i<sizeof(ns)/sizeof(*ns);i++) {
        JP(BPF_JMP|BPF_JEQ|BPF_K,ns[i],0,1);
        ST(BPF_RET|BPF_K,SECCOMP_RET_USER_NOTIF);
    }
    ST(BPF_RET|BPF_K,SECCOMP_RET_ALLOW);
    struct sock_fprog p={n,b};
    if(prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0))die("no_new_privs");
    int fd=syscall(SYS_seccomp,SECCOMP_SET_MODE_FILTER,SECCOMP_FILTER_FLAG_NEW_LISTENER,&p);
    if(fd<0)die("seccomp-v2");
    return fd;
}

/* 唯一启动通道临时占 fd 3，发送 listener 后关闭；其余继承 fd 在过滤器之前清理。 */
static void v2_launch(int sock,char **argv,pid_t parent) {
    if(prctl(PR_SET_PDEATHSIG,SIGKILL)||getppid()!=parent)_exit(125);
    setsid();
    uid_t uid=getuid();gid_t gid=getgid();char map[80];
    if(unshare(CLONE_NEWUSER))die("user namespace");
    writefile("/proc/self/setgroups","deny");
    snprintf(map,sizeof(map),"0 %u 1",uid);writefile("/proc/self/uid_map",map);
    snprintf(map,sizeof(map),"0 %u 1",gid);writefile("/proc/self/gid_map",map);
    if(unshare(CLONE_NEWNS|CLONE_NEWPID|CLONE_NEWNET))die("namespaces");
    if(mount(NULL,"/",NULL,MS_REC|MS_PRIVATE,NULL))die("private mounts");
    int parentfd=syscall(SYS_pidfd_open,parent,0),barrier[2];
    if(parentfd<0||pipe2(barrier,O_CLOEXEC))die("init 同步");
    pid_t p=syscall(SYS_clone,CLONE_PARENT|SIGCHLD,0,0,0,0);if(p<0)die("clone init");
    if(p) {
        if(send(sock,&p,sizeof(p),0)!=sizeof(p))die("send init");
        close(sock);close(barrier[0]);write(barrier[1],"R",1);close(barrier[1]);close(parentfd);
        int pfd=syscall(SYS_pidfd_open,p,0);struct pollfd done={pfd,POLLIN,0};
        while(poll(&done,1,-1)<0&&errno==EINTR){}
        _exit(0);
    }
    /* 新 PID namespace 看不见父进程；用启动 socket 的握手确认父进程尚存。 */
    if(prctl(PR_SET_PDEATHSIG,SIGKILL))die("init pdeathsig");
    struct pollfd alive={parentfd,POLLIN,0};if(poll(&alive,1,0)>0)_exit(125);
    close(parentfd);close(barrier[1]);char token;
    if(read(barrier[0],&token,1)!=1)_exit(125);
    close(barrier[0]);
    char ready='R';if(send(sock,&ready,1,MSG_NOSIGNAL)!=1)_exit(125);
    if(mount("proc","/proc","proc",MS_NOSUID|MS_NOEXEC|MS_NODEV,NULL))die("proc");
    p=fork();if(p<0)die("fork command");
    if(p) {
        close(sock);int st,result=0;pid_t w;
        while((w=waitpid(-1,&st,0))>0||errno==EINTR)
            if(w==p)result=WIFEXITED(st)?WEXITSTATUS(st):128+WTERMSIG(st);
        _exit(result);
    }
    if(chdir(root))die("cwd");
    if(sock!=3){if(dup3(sock,3,O_CLOEXEC)<0)die("启动通道");close(sock);}
    if(syscall(SYS_close_range,4,~0U,0))die("关闭继承 fd");
    int fd=v2_filter();sendfd(3,fd);close(fd);close(3);
    execvp(argv[0],argv);die("execvp");
}

struct request {
    int nr,d1,d2,flags,perm,isopen,kind,follow,err;
    uint64_t a[6];struct open_how how;
    char s[4096],t[4096];
};
/* kind: 1 rename, 2 unlink, 3 link, 4 symlink, 5 mkdir, 6 mknod, 7 chmod, 8 truncate. */
static int copy_request(struct seccomp_notif *n,struct request *o) {
    memset(o,0,sizeof(*o));o->nr=n->data.nr;memcpy(o->a,n->data.args,48);
    uint64_t *a=o->a;int i=0,j=-1;o->d1=o->d2=AT_FDCWD;
    switch(o->nr) {
    case SYS_open:o->isopen=1;o->flags=a[1];o->perm=a[2];break;
    case SYS_creat:o->isopen=1;o->flags=O_WRONLY|O_CREAT|O_TRUNC;o->perm=a[1];break;
    case SYS_openat:o->isopen=1;o->d1=a[0];i=1;o->flags=a[2];o->perm=a[3];break;
    case SYS_openat2:
        o->isopen=1;o->d1=a[0];i=1;
        if(a[3]<sizeof(o->how)){errno=EINVAL;return -1;}
        if(a[3]>4096){errno=E2BIG;return -1;}
        if(a[3]>sizeof(o->how)) {
            unsigned char extra[4096];size_t len=a[3]-sizeof(o->how);
            if(readmem(n->pid,a[2]+sizeof(o->how),extra,len,n->id)!=(int)len){errno=EFAULT;return -1;}
            for(size_t k=0;k<len;k++)if(extra[k]){errno=E2BIG;return -1;}
        }
        if(readmem(n->pid,a[2],&o->how,sizeof(o->how),n->id)!=sizeof(o->how)){errno=EFAULT;return -1;}
        o->flags=o->how.flags;o->perm=o->how.mode;break;
    case SYS_rename:o->kind=1;j=1;break;
    case SYS_renameat:case SYS_renameat2:o->kind=1;o->d1=a[0];i=1;o->d2=a[2];j=3;break;
    case SYS_unlink:case SYS_rmdir:o->kind=2;break;
    case SYS_unlinkat:o->kind=2;o->d1=a[0];i=1;break;
    case SYS_link:o->kind=3;j=1;break;
    case SYS_linkat:o->kind=3;o->d1=a[0];i=1;o->d2=a[2];j=3;break;
    case SYS_symlink:o->kind=4;j=1;break;
    case SYS_symlinkat:o->kind=4;o->d2=a[1];j=2;break;
    case SYS_mkdir:o->kind=5;o->perm=a[1];break;
    case SYS_mkdirat:o->kind=5;o->d1=a[0];i=1;o->perm=a[2];break;
    case SYS_mknod:o->kind=6;o->perm=a[1];break;
    case SYS_mknodat:o->kind=6;o->d1=a[0];i=1;o->perm=a[2];break;
    case SYS_chmod:o->kind=7;o->perm=a[1];break;
    case SYS_fchmod:o->kind=7;o->d1=a[0];o->perm=a[1];o->follow=1;return 0;
    case SYS_fchmodat:case SYS_fchmodat2:o->kind=7;o->d1=a[0];i=1;o->perm=a[2];break;
    case SYS_truncate:o->kind=8;break;
    default:errno=ENOSYS;return -1;
    }
    if(readstr(n->pid,a[i],o->s,n->id)){errno=EFAULT;return -1;}
    if(j>=0&&readstr(n->pid,a[j],o->t,n->id)){errno=EFAULT;return -1;}
    o->follow=(o->isopen&&!(o->flags&O_NOFOLLOW)&&!((o->flags&O_CREAT)&&(o->flags&O_EXCL)))||o->kind==8||o->kind==7||
        (o->nr==SYS_linkat&&(a[4]&AT_SYMLINK_FOLLOW));
    if(o->nr==SYS_fchmodat2&&(a[3]&AT_SYMLINK_NOFOLLOW))o->follow=0;
    return 0;
}
struct prepared {
    int err,exists1,exists2,tmpfile;
    char leaf1[4096],leaf2[4096];
    struct stat st1,st2;
};
struct outcome {long value;int err,mismatch,verified;};

static void send_packet(int sock,void *data,size_t len,int *fds,int count) {
    char control[CMSG_SPACE(5*sizeof(int))]={0};struct iovec iov={data,len};
    struct msghdr m={.msg_iov=&iov,.msg_iovlen=1};
    if(count){m.msg_control=control;m.msg_controllen=CMSG_SPACE(count*sizeof(int));
        struct cmsghdr *c=CMSG_FIRSTHDR(&m);c->cmsg_level=SOL_SOCKET;c->cmsg_type=SCM_RIGHTS;
        c->cmsg_len=CMSG_LEN(count*sizeof(int));memcpy(CMSG_DATA(c),fds,count*sizeof(int));}
    if(sendmsg(sock,&m,MSG_NOSIGNAL)!=(ssize_t)len)_exit(126);
}
static int recv_packet(int sock,void *data,size_t len,int *fds) {
    char control[CMSG_SPACE(5*sizeof(int))]={0};struct iovec iov={data,len};
    struct msghdr m={.msg_iov=&iov,.msg_iovlen=1,.msg_control=control,.msg_controllen=sizeof(control)};
    if(recvmsg(sock,&m,MSG_CMSG_CLOEXEC)!=(ssize_t)len)return -1;
    struct cmsghdr *c=CMSG_FIRSTHDR(&m);if(!c)return 0;
    int count=(c->cmsg_len-CMSG_LEN(0))/sizeof(int);memcpy(fds,CMSG_DATA(c),count*sizeof(int));return count;
}
static int proc_open(pid_t pid,const char *tail,int flags) {
    char p[120];snprintf(p,sizeof(p),"/proc/%d/%s",pid,tail);return open(p,flags|O_CLOEXEC);
}
static int dup_remote(pid_t pid,int fd) {
    int p=syscall(SYS_pidfd_open,pid,0);if(p<0)return -1;
    int r=syscall(SYS_pidfd_getfd,p,fd,0),e=errno;close(p);errno=e;return r;
}
static void rewrite_proc(char *s,int nspid) {
    char p[4096];const char *tail=NULL;
    if(!strncmp(s,"/proc/self/",11))tail=s+11;
    if(!strncmp(s,"/proc/thread-self/",18))tail=s+18;
    if(tail){snprintf(p,sizeof(p),"/proc/%d/%s",nspid,tail);strcpy(s,p);}
}
/* helper 在复制的上下文中操作，宿主保留账本与唯一策略归属。并发改变 fs/凭据仍不具备原子快照。 */
static int enter_context(pid_t pid,struct request *o) {
    char tail[80],line[512];int nspid=0;unsigned mask=022;unsigned long long effective=0;
    snprintf(tail,sizeof(tail),"/proc/%d/status",pid);FILE *f=fopen(tail,"r");if(!f)return -1;
    while(fgets(line,sizeof(line),f)) {
        if(!strncmp(line,"Umask:",6))sscanf(line+6,"%o",&mask);
        if(!strncmp(line,"CapEff:",7))sscanf(line+7,"%llx",&effective);
        if(!strncmp(line,"NSpid:",6)){char *s=line+6,*e;while(1){long n=strtol(s,&e,10);if(e==s)break;nspid=n;s=e;}}
    }
    fclose(f);
    int u=proc_open(pid,"ns/user",O_RDONLY),m=proc_open(pid,"ns/mnt",O_RDONLY);
    int r=proc_open(pid,"root",O_PATH|O_DIRECTORY),cwd=proc_open(pid,"cwd",O_PATH|O_DIRECTORY);
    if(u<0||m<0||r<0||cwd<0)return -1;
    if(o->d1!=AT_FDCWD){int d=dup_remote(pid,o->d1);if(d<0)return -1;o->d1=d;}
    if(o->d2!=AT_FDCWD){int d=dup_remote(pid,o->d2);if(d<0)return -1;o->d2=d;}
    if(setns(u,CLONE_NEWUSER)||setns(m,CLONE_NEWNS)||fchdir(r)||chroot(".")||fchdir(cwd))return -1;
    close(u);close(m);close(r);close(cwd);umask(mask);
    struct __user_cap_header_struct h={_LINUX_CAPABILITY_VERSION_3,0};
    struct __user_cap_data_struct c[2]={0};
    if(syscall(SYS_capget,&h,c))return -1;
    c[0].effective=(uint32_t)effective;c[1].effective=effective>>32;
    if(syscall(SYS_capset,&h,c))return -1;
    if(o->kind!=4)rewrite_proc(o->s,nspid);
    rewrite_proc(o->t,nspid);return 0;
}
static int native_open(struct request *o,int flags) {
    if(o->nr==SYS_openat2) {
        struct open_how h=o->how;h.flags=(h.flags&~(uint64_t)UINT32_MAX)|(unsigned int)flags;
        if(!(flags&O_CREAT)&&((flags&O_TMPFILE)!=O_TMPFILE))h.mode=0;
        return syscall(SYS_openat2,o->d1,o->s,&h,sizeof(h));
    }
    return openat(o->d1,o->s,flags,o->perm);
}
/* 父目录按内核解析后固定；对悬空末级链接递归解析。openat2 的缺失末端有单独负例覆盖。 */
static int parent_leaf(int d,const char *path,int follow,int depth,int *parent,char *leaf) {
    if(depth>40){errno=ELOOP;return -1;}
    char copy[4096];snprintf(copy,sizeof(copy),"%s",path);
    if(!*copy){errno=ENOENT;return -1;}
    char *s=strrchr(copy,'/');
    if(s){strcpy(leaf,s+1);*s=0;*parent=openat(d,*copy?copy:"/",O_PATH|O_DIRECTORY|O_CLOEXEC);}
    else {strcpy(leaf,copy);*parent=openat(d,".",O_PATH|O_DIRECTORY|O_CLOEXEC);}
    if(*parent<0)return -1;
    if(follow) {
        char link[4096];ssize_t n=readlinkat(*parent,leaf,link,sizeof(link)-1);
        if(n>=0){link[n]=0;int old=*parent;int ret=parent_leaf(old,link,1,depth+1,parent,leaf);close(old);return ret;}
    }
    return 0;
}
static int same_inode(struct stat *a,struct stat *b){return a->st_dev==b->st_dev&&a->st_ino==b->st_ino;}
static void helper(int sock,pid_t pid,struct request o) {
    pid_t parent=getppid();prctl(PR_SET_PDEATHSIG,SIGKILL);if(getppid()!=parent)_exit(125);
    /* 不把监听器、日志或标准流带入受控 worker 的上下文。 */
    if(listener>=0)close(listener);
    close(journalfd);close(statefd);close(0);close(1);close(2);
    struct prepared p={0};int fds[5]={-1,-1,-1,-1,-1};
    if(enter_context(pid,&o))goto prepare_error;
    if(o.kind==5) {size_t len=strlen(o.s);while(len>1&&o.s[len-1]=='/')o.s[--len]=0;}
    if(o.isopen) {
        p.tmpfile=(o.flags&O_TMPFILE)==O_TMPFILE;
        if(p.tmpfile){fds[0]=native_open(&o,o.flags);if(fds[0]<0)goto prepare_error;p.exists1=1;}
        else {
            /* O_EXCL 与 O_NOFOLLOW 在真实解析中保留，不先 realpath。 */
            int fl=o.flags&~(O_CREAT|O_TRUNC);
            if((o.flags&O_CREAT)&&(o.flags&O_EXCL)) {
                int test=native_open(&o,O_PATH|O_NOFOLLOW);
                if(test>=0){close(test);errno=EEXIST;goto prepare_error;}
                if(errno!=ENOENT)goto prepare_error;
                fl|=O_NOFOLLOW;
            }
            fds[0]=native_open(&o,fl);
            if(fds[0]>=0){p.exists1=1;fstat(fds[0],&p.st1);
                if((o.flags&O_CREAT)&&(o.flags&O_EXCL)){errno=EEXIST;goto prepare_error;}
                if((o.flags&O_TRUNC)&&(o.flags&O_ACCMODE)==O_RDONLY){
                    int w=native_open(&o,(fl&~O_ACCMODE)|O_WRONLY);if(w<0)goto prepare_error;
                    struct stat st;fstat(w,&st);if(!same_inode(&st,&p.st1)){close(w);errno=ESTALE;goto prepare_error;}
                    fds[4]=w;
                }
            } else if(errno==ENOENT&&(o.flags&O_CREAT)) {
                if(o.nr==SYS_openat2&&o.how.resolve) {
                    char path[4096];strcpy(path,o.s);
                    for(int depth=0;;depth++) {
                        if(depth>40){errno=ELOOP;goto prepare_error;}
                        char dir[4096];strcpy(dir,path);char *slash=strrchr(dir,'/');
                        if(slash){strcpy(p.leaf1,slash+1);*slash=0;if(!dir[0])strcpy(dir,"/");}
                        else{strcpy(p.leaf1,dir);strcpy(dir,".");}
                        struct open_how h={.flags=O_PATH|O_DIRECTORY,.resolve=o.how.resolve};
                        fds[2]=syscall(SYS_openat2,o.d1,dir,&h,sizeof(h));
                        if(fds[2]<0)goto prepare_error;
                        char target[4096];ssize_t len=readlinkat(fds[2],p.leaf1,target,4095);
                        if(len<0)break;
                        target[len]=0;close(fds[2]);fds[2]=-1;
                        if(o.how.resolve&RESOLVE_NO_SYMLINKS){errno=ELOOP;goto prepare_error;}
                        if(target[0]=='/')strcpy(path,target);
                        else snprintf(path,sizeof(path),"%s/%s",dir,target);
                        /* 重新以原边界验证每个替代路径，再绑定不存在末端的父目录。 */
                        struct open_how check={.flags=O_PATH,.resolve=o.how.resolve};
                        int probe=syscall(SYS_openat2,o.d1,path,&check,sizeof(check));
                        if(probe>=0){close(probe);errno=EAGAIN;goto prepare_error;}
                        if(errno!=ENOENT)goto prepare_error;
                    }
                }else if(parent_leaf(o.d1,o.s,o.follow,0,&fds[2],p.leaf1))goto prepare_error;
                /* 原路径的 resolve 已由第一次 native_open 检查；创建再使用同一 resolve 约束。 */
            } else goto prepare_error;
        }
    } else if(o.nr==SYS_fchmod) {
        fds[0]=o.d1;p.exists1=1;
    } else {
        if(o.kind!=4) {
            if(o.kind==3&&o.nr==SYS_linkat&&!o.s[0]&&(o.a[4]&AT_EMPTY_PATH)) {
                fds[0]=o.d1;p.exists1=1;
            } else {
                if(parent_leaf(o.d1,o.s,o.kind==3?0:o.follow,0,&fds[2],p.leaf1))goto prepare_error;
                fds[0]=openat(fds[2],p.leaf1,O_PATH|((o.kind==3&&o.follow)?0:O_NOFOLLOW)|O_CLOEXEC);
                p.exists1=fds[0]>=0;if(!p.exists1&&errno!=ENOENT)goto prepare_error;
            }
        }
        if(o.kind==1||o.kind==3||o.kind==4) {
            if(parent_leaf(o.d2,o.t,0,0,&fds[3],p.leaf2))goto prepare_error;
            fds[1]=openat(fds[3],p.leaf2,O_PATH|O_NOFOLLOW|O_CLOEXEC);
            p.exists2=fds[1]>=0;if(!p.exists2&&errno!=ENOENT)goto prepare_error;
        }
        if(o.kind==8){int w=openat(fds[2],p.leaf1,O_WRONLY|O_NOFOLLOW);if(w<0)goto prepare_error;
            struct stat st,old;fstat(w,&st);fstat(fds[0],&old);
            if(!same_inode(&st,&old)){close(w);errno=ESTALE;goto prepare_error;}close(fds[0]);fds[0]=w;}
    }
    if(p.exists1)fstat(fds[0],&p.st1);
    if(p.exists2)fstat(fds[1],&p.st2);
    /* 无对象的位置传 socket 副本占位，chroot 中也无需 /dev。 */
    for(int i=0;i<5;i++)if(fds[i]<0)fds[i]=dup(sock);
    send_packet(sock,&p,sizeof(p),fds,5);
    int ok;if(recv(sock,&ok,sizeof(ok),0)!=sizeof(ok)||!ok)_exit(0);
    struct outcome result={.value=-1};long v=-1;
    if(o.isopen) {
        if(p.exists1){v=fds[0];
            if((o.flags&O_TRUNC)&&S_ISREG(p.st1.st_mode)&&ftruncate((o.flags&O_ACCMODE)==O_RDONLY?fds[4]:fds[0],0))v=-1;
        }else {
            /* 原 resolve 约束已用于解析父目录与末级链接；在固定父目录下排他创建。 */
            v=openat(fds[2],p.leaf1,o.flags|O_EXCL,o.perm);
            if(v>=0){struct stat a,b;fstat(v,&a);if(fstatat(fds[2],p.leaf1,&b,AT_SYMLINK_NOFOLLOW)||!same_inode(&a,&b))result.mismatch=1;}
        }
    } else switch(o.kind) {
    case 1:v=syscall(SYS_renameat2,fds[2],p.leaf1,fds[3],p.leaf2,o.nr==SYS_renameat2?o.a[4]:0);
        break;
    case 2:v=unlinkat(fds[2],p.leaf1,(o.nr==SYS_rmdir||(o.nr==SYS_unlinkat&&(o.a[2]&AT_REMOVEDIR)))?AT_REMOVEDIR:0);break;
    case 3:
        if(o.nr==SYS_linkat&&!o.s[0])v=linkat(fds[0],"",fds[3],p.leaf2,AT_EMPTY_PATH);
        else v=linkat(fds[2],p.leaf1,fds[3],p.leaf2,o.nr==SYS_linkat?o.a[4]:0);
        break;
    case 4:v=symlinkat(o.s,fds[3],p.leaf2);break;
    case 5:v=mkdirat(fds[2],p.leaf1,o.perm);break;
    case 6:v=mknodat(fds[2],p.leaf1,o.perm,o.nr==SYS_mknod?o.a[2]:o.a[3]);break;
    case 7:v=syscall(SYS_fchmodat2,fds[0],"",o.perm,AT_EMPTY_PATH|(o.follow?0:AT_SYMLINK_NOFOLLOW));break;
    case 8:v=ftruncate(fds[0],o.a[1]);break;
    }
    result.err=v<0?errno:0;result.value=v;
    if(v>=0&&race&&getenv("WI_RACE_PHASE")&&atoi(getenv("WI_RACE_PHASE"))==2&&
       (strstr(o.s,"race-safe")||strstr(o.t,"race-safe"))&&__atomic_load_n(race,__ATOMIC_ACQUIRE)==1) {
        __atomic_store_n(race,2,__ATOMIC_RELEASE);double end=now()+5;
        while(__atomic_load_n(race,__ATOMIC_ACQUIRE)!=3&&now()<end)sched_yield();
    }
    if(v>=0&&o.kind>=1&&o.kind<=4) {
        struct stat a,b;result.verified=1;
        if(o.kind==1||o.kind==3) {
            if(fstatat(fds[3],p.leaf2,&a,AT_SYMLINK_NOFOLLOW)||!same_inode(&a,&p.st1))result.mismatch=1;
            if(o.kind==1&&o.nr==SYS_renameat2&&(o.a[4]&RENAME_EXCHANGE))
                if(fstatat(fds[2],p.leaf1,&b,AT_SYMLINK_NOFOLLOW)||!same_inode(&b,&p.st2))result.mismatch=1;
        } else if(o.kind==2) {
            /* 删除后无法通过路径取得已删除对象。nlink 是可检验但不充分的见证；ABA 探针展示局限。 */
            if(fstat(fds[0],&a)||fstatat(fds[2],p.leaf1,&b,AT_SYMLINK_NOFOLLOW)==0||
               a.st_nlink!=(S_ISDIR(p.st1.st_mode)?0:p.st1.st_nlink-1))result.mismatch=1;
        } else {
            char link[4096];ssize_t n=readlinkat(fds[3],p.leaf2,link,4095);
            if(n<0)result.mismatch=1;else{link[n]=0;if(strcmp(link,o.s))result.mismatch=1;}
        }
    }
    int fd=(int)v;send_packet(sock,&result,sizeof(result),&fd,o.isopen&&v>=0?1:0);_exit(0);
prepare_error:
    p.err=errno?errno:EIO;send_packet(sock,&p,sizeof(p),NULL,0);_exit(0);
}

static int category(const char *p) {
    size_t len=strlen(root);if(strncmp(p,root,len)||(p[len]&&p[len]!='/'))return 0;
    const char *s=p+len;if(*s=='/')s++;
    if(!strcmp(s,"hidden")||!strncmp(s,"hidden/",7))return -1;
    if(!strcmp(s,"target")||!strncmp(s,"target/",7))return 2;
    if(!strcmp(s,".git")||!strncmp(s,".git/",5))return 3;
    return 1;
}
static void fdpath(int fd,char *p) {
    char f[80];snprintf(f,sizeof(f),"/proc/self/fd/%d",fd);if(rl(f,p))p[0]=0;
}
static int cp_object(const char *p,int fd,int exists,int force) {
    struct stat st={0};if(exists&&fstat(fd,&st))return -1;
    int c=category(p);if(c==-1){errno=EACCES;return -1;}
    if(!force&&(c!=1&&!(c>1&&exists&&S_ISREG(st.st_mode)&&st.st_nlink>1)))return 0;
    for(size_t i=0;i<npaths;i++)if(!strcmp(paths[i],p))return 0;
    char clone[4096],link[4096]="";const char *type="Absent";
    if(exists) {
        if(S_ISREG(st.st_mode)) {
            type="file";char proc[80];snprintf(proc,sizeof(proc),"/proc/self/fd/%d",fd);
            int src=open(proc,O_RDONLY|O_CLOEXEC);snprintf(clone,sizeof(clone),"%s/%zu.clone",state,npaths);
            int dst=open(clone,O_WRONLY|O_CREAT|O_EXCL|O_CLOEXEC,0600);
            if(src<0||dst<0||ioctl(dst,FICLONE,src)||fsync(dst)){int e=errno;if(src>=0)close(src);if(dst>=0)close(dst);errno=e;errors++;return -1;}
            close(src);close(dst);clones++;
        }else if(S_ISDIR(st.st_mode))type="directory";
        else if(S_ISLNK(st.st_mode)){type="symlink";ssize_t n=readlinkat(fd,"",link,4095);if(n<0)return -1;link[n]=0;}
        else if(S_ISFIFO(st.st_mode))type="fifo";
        else {errno=ENOTSUP;return -1;}
    }
    if(npaths==cap){cap=cap?cap*2:64;paths=realloc(paths,cap*sizeof(*paths));if(!paths)die("账本内存");}
    paths[npaths]=strdup(p);
    fprintf(ledger,"{\"index\":%zu,\"path\":",npaths);jsonstr(ledger,p);
    fprintf(ledger,",\"before\":\"%s\",\"mode\":%u,\"dev\":%lu,\"ino\":%lu,\"nlink\":%lu,\"link\":",type,st.st_mode,st.st_dev,st.st_ino,st.st_nlink);
    jsonstr(ledger,link);fprintf(ledger,"}\n");fflush(ledger);npaths++;
    if(!getenv("WI_BATCH")&&(fsync(journalfd)||fsync(statefd)))die("持久账本");
    return 0;
}
static int cp_tree(int fd,const char *p,const char *q) {
    if(cp_object(p,fd,1,0)||cp_object(q,-1,0,0))return -1;
    struct stat st;if(fstat(fd,&st))return -1;if(!S_ISDIR(st.st_mode))return 0;
    if(category(p)!=1&&category(q)!=1)return 0;
    char proc[80];snprintf(proc,sizeof(proc),"/proc/self/fd/%d",fd);DIR *d=opendir(proc);if(!d)return -1;
    int ret=0;struct dirent *e;while((e=readdir(d))) {
        if(!strcmp(e->d_name,".")||!strcmp(e->d_name,".."))continue;
        char a[4096],b[4096];snprintf(a,sizeof(a),"%s/%s",p,e->d_name);snprintf(b,sizeof(b),"%s/%s",q,e->d_name);
        int sub=openat(dirfd(d),e->d_name,O_PATH|O_NOFOLLOW|O_CLOEXEC);
        if(sub<0||cp_tree(sub,a,b))ret=-1;
        if(sub>=0)close(sub);
        if(ret)break;
    }closedir(d);return ret;
}
static void mark_uncertain(const char *reason) {
    uncertain=1;char p[4096];snprintf(p,sizeof(p),"%s/indeterminate.json",state);FILE *f=fopen(p,"w");
    fprintf(f,"{\"outcome\":\"Indeterminate\",\"reason\":");jsonstr(f,reason);fprintf(f,"}\n");fflush(f);fsync(fileno(f));fclose(f);
    if(initpid>0)kill(initpid,SIGKILL);
}
static void one_notification(void);
static void v2_rendezvous(const char *p,int phase) {
    int wanted=getenv("WI_RACE_PHASE")?atoi(getenv("WI_RACE_PHASE")):0;
    if(phase!=wanted||!race||!strstr(p,"race-safe")||__atomic_load_n(race,__ATOMIC_ACQUIRE)!=1)return;
    __atomic_store_n(race,2,__ATOMIC_RELEASE);double end=now()+5;
    while(__atomic_load_n(race,__ATOMIC_ACQUIRE)!=3&&now()<end&&!uncertain&&!timedout) {
        struct pollfd f={listener,POLLIN,0};if(poll(&f,1,0)>0&&(f.revents&POLLIN))one_notification();else sched_yield();
    }race_done++;
}
static void one_notification(void) {
    struct seccomp_notif n={0};if(ioctl(listener,SECCOMP_IOCTL_NOTIF_RECV,&n))return;calls++;
    struct seccomp_notif_resp reply={.id=n.id};struct request o;
    if(copy_request(&n,&o)){reply.error=-errno;goto send_reply;}
    if(o.kind==3)link_calls++;
    int socks[2];if(socketpair(AF_UNIX,SOCK_SEQPACKET|SOCK_CLOEXEC,0,socks))die("worker socket");
    pid_t w=fork();if(w<0)die("worker fork");
    if(!w){close(socks[0]);helper(socks[1],n.pid,o);}
    worker_count++;close(socks[1]);struct prepared p;int fds[5];int count=recv_packet(socks[0],&p,sizeof(p),fds);
    if(count<0){reply.error=-EIO;goto finish;}
    if(p.err){reply.error=-p.err;goto finish;}
    if(count!=5){reply.error=-EIO;goto finish;}
    char a[4096]="",b[4096]="",base[4096];
    if(p.exists1)fdpath(fds[0],a);else if(o.kind!=4){fdpath(fds[2],base);snprintf(a,sizeof(a),"%s/%s",base,p.leaf1);}
    if(o.kind==1||o.kind==3||o.kind==4){fdpath(fds[3],base);snprintf(b,sizeof(b),"%s/%s",base,p.leaf2);}
    int ok=1;
    if(category(a)==-1||(b[0]&&category(b)==-1)){errno=EACCES;ok=0;}
    if(ok&&o.kind==3&&category(a)!=category(b)&&category(a)>0&&category(b)>0){errno=EXDEV;ok=0;}
    if(ok&&!p.tmpfile) {
        if(o.kind==1) {
            /* 先保存目的端，再遍历源目录，避免把已有目的端登记为 Absent。 */
            if(cp_object(b,fds[1],p.exists2,0)||cp_tree(fds[0],a,b))ok=0;
            if(ok&&o.nr==SYS_renameat2&&(o.a[4]&RENAME_EXCHANGE)&&cp_tree(fds[1],b,a))ok=0;
        }else if(o.kind==3||o.kind==4){if(cp_object(b,fds[1],p.exists2,0))ok=0;}
        else if(cp_object(a,fds[0],p.exists1,0))ok=0;
    }
    if(ok&&getenv("WI_BATCH")&&(fsync(journalfd)||fsync(statefd)))die("批量持久账本");
    if(!ok){reply.error=-errno;denied++;}
    if(ok)v2_rendezvous(a[0]?a:b,0);
    if(ok&&ioctl(listener,SECCOMP_IOCTL_NOTIF_ID_VALID,&n.id)){ok=0;reply.error=-ENOENT;}
    if(send(socks[0],&ok,sizeof(ok),MSG_NOSIGNAL)!=sizeof(ok)){reply.error=-EIO;ok=0;}
    if(ok) {
        struct outcome r;int resultfd=-1;int got=recv_packet(socks[0],&r,sizeof(r),&resultfd);emulated++;
        if(got<0){mark_uncertain("代执行 worker 未返回结果");reply.error=-EIO;}
        else {
            checks+=r.verified;mismatches+=r.mismatch;
            if(o.kind==3&&r.value>=0)link_success++;
            if(r.mismatch)mark_uncertain("目录项或创建后的 inode 与检查点不一致");
            v2_rendezvous(a,1);
            if(r.value<0)reply.error=-r.err;
            else if(o.isopen&&got==1) {
                struct seccomp_notif_addfd ad={.id=n.id,.flags=SECCOMP_ADDFD_FLAG_SEND,.srcfd=resultfd,.newfd_flags=(o.flags&O_CLOEXEC)?O_CLOEXEC:0};
                if(ioctl(listener,SECCOMP_IOCTL_NOTIF_ADDFD,&ad)<0){reply_failures++;mark_uncertain("代执行已完成，但 ADDFD 失败或通知已取消");}
                close(resultfd);reply.id=0;
            } else reply.val=r.value;
        }
    }
    for(int i=0;i<count;i++)close(fds[i]);
finish:
    close(socks[0]);int st;while(waitpid(w,&st,0)<0&&errno==EINTR){}
send_reply:
    if(getenv("WI_TRACE")&&reply.error) {
        char logpath[4096];snprintf(logpath,sizeof(logpath),"%s/errors.jsonl",state);FILE *log=fopen(logpath,"a");
        fprintf(log,"{\"nr\":%d,\"errno\":%d,\"source\":",o.nr,-reply.error);jsonstr(log,o.s);
        fprintf(log,",\"target\":");jsonstr(log,o.t);fprintf(log,"}\n");fclose(log);
    }
    if(reply.id&&ioctl(listener,SECCOMP_IOCTL_NOTIF_SEND,&reply)) {
        if(!reply.error){reply_failures++;mark_uncertain("代执行已完成，但回复失败或通知已取消");}
    }
}
int main(int argc,char **argv) {
    if(argc<7)return 2;
    if(strcmp(argv[1],"B2v2"))return baseline_main(argc,argv);
    mode=argv[1];if(!realpath(argv[2],root)||!realpath(argv[3],state))die("目录");
    char p[4096];snprintf(p,sizeof(p),"%s/ledger.jsonl",state);ledger=fopen(p,"w");if(!ledger)die("账本");
    journalfd=fileno(ledger);statefd=open(state,O_DIRECTORY|O_CLOEXEC);
    if(getenv("WI_RACE_CONTROL")){int fd=open(getenv("WI_RACE_CONTROL"),O_RDWR|O_CLOEXEC);if(fd<0)die("竞态控制");race=mmap(NULL,4096,PROT_READ|PROT_WRITE,MAP_SHARED,fd,0);close(fd);}
    prctl(PR_SET_CHILD_SUBREAPER,1);struct sigaction sa={.sa_handler=timeout_handler};sigaction(SIGALRM,&sa,NULL);
    /* 三个标准流必须是节点提供的管道；实验 runner 创建并收集这些管道。 */
    for(int i=0;i<3;i++){struct stat st;if(fstat(i,&st)||!S_ISFIFO(st.st_mode)){fprintf(stderr,"标准流必须是管道\n");return 125;}}
    int socks[2];if(socketpair(AF_UNIX,SOCK_SEQPACKET|SOCK_CLOEXEC,0,socks))die("启动 socket");
    double start=now();pid_t parent=getpid();launchpid=fork();if(launchpid<0)die("fork");
    if(!launchpid){close(socks[0]);fclose(ledger);close(statefd);v2_launch(socks[1],argv+6,parent);}
    close(socks[1]);alarm(atoi(argv[4]));pid_t init;
    if(recv(socks[0],&init,sizeof(init),0)!=sizeof(init))die("init pid");
    initpid=init;
    char ready;if(recv(socks[0],&ready,1,0)!=1)die("init ready");
    listener=recvfd(socks[0]);if(listener<0)die("listener");close(socks[0]);
    int pidfd=syscall(SYS_pidfd_open,launchpid,0);if(pidfd<0)die("pidfd");
    struct pollfd f[2]={{listener,POLLIN,0},{pidfd,POLLIN,0}};
    while(!timedout&&!uncertain){int r=poll(f,2,-1);if(r<0){if(errno==EINTR)continue;die("poll");}if(f[1].revents)break;if(f[0].revents&POLLIN)one_notification();}
    if(uncertain&&initpid>0)kill(initpid,SIGKILL);
    int st,code=0;while(waitpid(launchpid,&st,0)<0&&errno==EINTR){}
    while(waitpid(initpid,&st,0)<0&&errno==EINTR){}
    code=WIFEXITED(st)?WEXITSTATUS(st):128+WTERMSIG(st);while(waitpid(-1,&st,0)>0||errno==EINTR){}
    alarm(0);close(listener);close(pidfd);fclose(ledger);close(statefd);
    snprintf(p,sizeof(p),"%s/stats.json",state);FILE *fout=fopen(p,"w");
    fprintf(fout,"{\"mode\":\"B2v2\",\"wall_s\":%.9f,\"code\":%d,\"calls\":%lu,\"registered\":%zu,\"clones\":%lu,\"denied\":%lu,\"errors\":%lu,\"emulated\":%lu,\"workers\":%d,\"link_calls\":%lu,\"link_success\":%lu,\"inode_checks\":%d,\"mismatches\":%d,\"reply_failures\":%d,\"indeterminate\":%d,\"timeout\":%d,\"race_handshakes\":%d}\n",now()-start,code,calls,npaths,clones,denied,errors,emulated,worker_count,link_calls,link_success,checks,mismatches,reply_failures,uncertain,timedout,race_done);
    fclose(fout);return timedout?124:uncertain?120:code;
}
