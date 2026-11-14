/* 节点是账本、策略和锁的唯一持有者；worker 只执行复制的请求。 */
struct worker_job {pid_t pid;struct request request;};
static int channels[V3_SLOTS][2];
static pid_t worker_pid;
static int anchorfd=-1;
static pthread_rwlock_t workspace_lock=PTHREAD_RWLOCK_INITIALIZER;
static pthread_mutex_t directory_locks[257];
static pthread_mutex_t queue_mu=PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t queue_cv=PTHREAD_COND_INITIALIZER;
struct queued_request {struct seccomp_notif notification;struct request request;};
static struct queued_request queue[4096];
static size_t qread,qwrite;
static int stopping;
static void *worker_thread(void *arg) {
    int sock=(int)(intptr_t)arg;
    if(unshare(CLONE_FS))die("worker 线程 fs 上下文");
    struct worker_job j;
    while(recv(sock,&j,sizeof(j),0)==sizeof(j))helper(sock,j.pid,j.request);
    return NULL;
}
static void start_worker(void) {
    for(int i=0;i<V3_SLOTS;i++)if(socketpair(AF_UNIX,SOCK_SEQPACKET|SOCK_CLOEXEC,0,channels[i]))die("worker socket");
    worker_pid=fork();if(worker_pid<0)die("worker fork");
    if(!worker_pid) {
        pid_t parent=getppid();prctl(PR_SET_PDEATHSIG,SIGKILL);if(getppid()!=parent)_exit(125);
        hostproc=open("/proc",O_PATH|O_DIRECTORY|O_CLOEXEC);
        int u=proc_open(initpid,"ns/user",O_RDONLY),m=proc_open(initpid,"ns/mnt",O_RDONLY);
        if(u<0||m<0||setns(u,CLONE_NEWUSER)||setns(m,CLONE_NEWNS))die("worker namespace");
        close(u);close(m);
        worker_root=open("/",O_PATH|O_DIRECTORY|O_CLOEXEC);
        struct __user_cap_header_struct h={_LINUX_CAPABILITY_VERSION_3,0};syscall(SYS_capget,&h,worker_caps);
        close(listener);close(journalfd);close(statefd);close(anchorfd);if(audit)close(fileno(audit));
        pthread_t threads[V3_SLOTS];
        for(int i=0;i<V3_SLOTS;i++){close(channels[i][0]);pthread_create(&threads[i],NULL,worker_thread,(void*)(intptr_t)channels[i][1]);}
        for(int i=0;i<V3_SLOTS;i++)pthread_join(threads[i],NULL);
        _exit(0);
    }
    for(int i=0;i<V3_SLOTS;i++)close(channels[i][1]);
    worker_count=1;
}
/* 快路只对能以目标 root/cwd/dirfd 解析的路径做保守分类，LL 最终约束实际参数。 */
static int resolve_path(pid_t pid,int dir,const char *s,int follow,char *out) {
    if(!s[0])return -1;
    char proc[128];
    if(s[0]=='/')snprintf(proc,sizeof(proc),"/proc/%d/root",pid);
    else if(dir==AT_FDCWD)snprintf(proc,sizeof(proc),"/proc/%d/cwd",pid);
    else snprintf(proc,sizeof(proc),"/proc/%d/fd/%d",pid,dir);
    int base=open(proc,O_PATH|O_DIRECTORY|O_CLOEXEC);
    if(base<0)return -1;
    const char *path=s[0]=='/'?s+1:s;
    int object=openat(base,path,O_PATH|O_CLOEXEC|(follow?0:O_NOFOLLOW));
    if(object>=0){fdpath(object,out);close(object);close(base);return out[0]?0:-1;}
    if(errno!=ENOENT){close(base);return -1;}
    /* 缺失末端只解析一次父目录；悬空链接交回完整代执行路径。 */
    struct stat st;
    if(!fstatat(base,path,&st,AT_SYMLINK_NOFOLLOW)&&S_ISLNK(st.st_mode)){close(base);return -1;}
    char parent_path[4096];snprintf(parent_path,sizeof(parent_path),"%s",path);
    char *slash=strrchr(parent_path,'/');const char *leaf=path;int parent;
    if(slash){*slash=0;leaf=slash+1;parent=openat(base,*parent_path?parent_path:".",O_PATH|O_DIRECTORY|O_CLOEXEC);}
    else parent=dup(base);
    close(base);if(parent<0)return -1;
    char parent_name[4096];fdpath(parent,parent_name);close(parent);
    if(!parent_name[0]||snprintf(out,4096,"%s/%s",parent_name,leaf)>=4096)return -1;
    return 0;
}

static int allowed_path(const char *p) {
    if(category(p)==1||category(p)==-1)return 0;
    for(int i=0;i<ngrants;i++) {
        struct writable_root *g=&grants[i];size_t len=strlen(g->path);
        if(strncmp(p,g->path,len)|| (p[len]&&(!g->directory||p[len]!='/')))continue;
        struct stat st;if(stat(g->path,&st)||st.st_dev!=g->dev||st.st_ino!=g->ino)continue;
        if(!stat(p,&st)&&S_ISREG(st.st_mode)&&st.st_nlink>1)return 0;
        return 1;
    }return 0;
}
static int fast_policy(pid_t pid,struct request *o,char *a,char *b) {
    if(o->kind==7 || (o->nr==SYS_openat2 && o->how.resolve))return 0;
    if(o->kind!=4&&resolve_path(pid,o->d1,o->s,o->follow,a))return 0;
    if((o->kind==1||o->kind==3||o->kind==4)&&resolve_path(pid,o->d2,o->t,0,b))return 0;
    if(category(a)==-1||category(b)==-1)return -EACCES;
    if((o->kind==1||o->kind==3)&&category(a)>0&&category(b)>0&&category(a)!=category(b))return 0;
    if(getenv("WI_FORCE_EMULATE"))return 0;
    return (o->kind==4||allowed_path(a))&&(!b[0]||allowed_path(b));
}
static void v3_rendezvous(const char *p,int phase) {
    int wanted=getenv("WI_RACE_PHASE")?atoi(getenv("WI_RACE_PHASE")):0;
    if(phase!=wanted||!race||!strstr(p,"race-safe")||__atomic_load_n(race,__ATOMIC_ACQUIRE)!=1)return;
    if(!__sync_bool_compare_and_swap(race,1,2))return;
    double end=now()+(getenv("WI_INTERNAL_LOCK")?0.03:5);
    while(__atomic_load_n(race,__ATOMIC_ACQUIRE)!=3&&now()<end&&!timedout)sched_yield();
    if(getenv("WI_INTERNAL_LOCK"))__atomic_store_n(race+3,__atomic_load_n(race+2,__ATOMIC_ACQUIRE),__ATOMIC_RELEASE);
    __sync_fetch_and_add(&race_done,1);
}
static int lock_number(int fd) {struct stat st;if(fstat(fd,&st))die("目录锁 fstat");return (st.st_ino^st.st_dev)%257;}
static void take_lock(int n) {if(pthread_mutex_trylock(&directory_locks[n])){__sync_fetch_and_add(&lock_waits,1);pthread_mutex_lock(&directory_locks[n]);}}
static int anchor_object(int fd,struct stat *st) {
    if(!S_ISREG(st->st_mode)||st->st_nlink<=1)return 0;
    char name[128],proc[80];snprintf(name,sizeof(name),"%lu-%lu.anchor",st->st_dev,st->st_ino);
    struct stat found;if(!fstatat(statefd,name,&found,0))return 0;
    snprintf(proc,sizeof(proc),"/proc/self/fd/%d",fd);
    if(linkat(AT_FDCWD,proc,anchorfd,name,AT_SYMLINK_FOLLOW))return -1;
    anchors++;return 1;
}
static void record_move(const char *a,const char *b,struct stat *st,struct stat *target,int exchange) {
    fprintf(ledger,"{\"record\":\"rename\",\"order\":%zu,\"from\":",npaths);jsonstr(ledger,a);
    fprintf(ledger,",\"to\":");jsonstr(ledger,b);
    fprintf(ledger,",\"dev\":%lu,\"ino\":%lu,\"exchange\":%d,\"target_dev\":%lu,\"target_ino\":%lu}\n",st->st_dev,st->st_ino,exchange,target->st_dev,target->st_ino);fflush(ledger);moves++;
}
static void abort_prepare(int sock,int *fds,int count) {
    struct execute_permission ep={0};send(sock,&ep,sizeof(ep),MSG_NOSIGNAL);
    for(int i=0;i<count;i++)close(fds[i]);
}
static void notification(int slot,struct seccomp_notif n,struct request o) {
    __sync_fetch_and_add(&calls,1);
    struct seccomp_notif_resp reply={.id=n.id};
    char a[4096]="",b[4096]="",base[4096];
    int sock=channels[slot][0],count=0,fds[5]={-1,-1,-1,-1,-1},l1=-1,l2=-1;
    struct prepared p;struct worker_job job={.pid=n.pid,.request=o};
    /* 所有 rename 先取工作区独占锁；目录 rename 必须覆盖整个子树。 */
    if(o.kind==1){if(pthread_rwlock_trywrlock(&workspace_lock)){__sync_fetch_and_add(&lock_waits,1);pthread_rwlock_wrlock(&workspace_lock);}}
    else {if(pthread_rwlock_tryrdlock(&workspace_lock)){__sync_fetch_and_add(&lock_waits,1);pthread_rwlock_rdlock(&workspace_lock);}}
    send_packet(sock,&job,sizeof(job),NULL,0);count=recv_packet(sock,&p,sizeof(p),fds);
    if(count<0||p.err||count!=5){reply.error=count<0?-EIO:-p.err;goto unlock;}
    int entry=o.kind>=1&&o.kind<=6;
    if(o.isopen&&!p.exists1)entry=1;
    if(entry) {
        if(o.kind!=4)l1=lock_number(fds[2]);
        if(o.kind==1||o.kind==3||o.kind==4)l2=lock_number(fds[3]);
        if(l1==l2)l2=-1;
        if(l2>=0&&(l1<0||l2<l1)){int swap=l1;l1=l2;l2=swap;}
        if(l1>=0)take_lock(l1);
        if(l2>=0)take_lock(l2);
        /* 在锁内重新读取对象；等待锁期间的合法修改不能成为陈旧前态。 */
        abort_prepare(sock,fds,count);
        send_packet(sock,&job,sizeof(job),NULL,0);count=recv_packet(sock,&p,sizeof(p),fds);
        if(count<0||p.err||count!=5){reply.error=count<0?-EIO:-p.err;goto unlock;}
        int k1=o.kind!=4?lock_number(fds[2]):-1,k2=(o.kind==1||o.kind==3||o.kind==4)?lock_number(fds[3]):-1;
        if((k1>=0&&k1!=l1&&k1!=l2)||(k2>=0&&k2!=l1&&k2!=l2)){
            abort_prepare(sock,fds,count);count=0;reply.error=-ESTALE;goto unlock;
        }
    }
    a[0]=b[0]=0;
    if(p.exists1)fdpath(fds[0],a);else if(o.kind!=4){fdpath(fds[2],base);snprintf(a,sizeof(a),"%s/%s",base,p.leaf1);}
    if(o.kind==1||o.kind==3||o.kind==4){fdpath(fds[3],base);snprintf(b,sizeof(b),"%s/%s",base,p.leaf2);}
    struct execute_permission ep={.ok=1};
    if(category(a)==-1||(b[0]&&category(b)==-1)){errno=EACCES;ep.ok=0;}
    if(ep.ok&&o.kind==3&&category(a)!=category(b)&&category(a)>0&&category(b)>0){errno=EXDEV;ep.ok=0;}
    pthread_mutex_lock(&ledger_mu);
    size_t previous=npaths;unsigned long oldmoves=moves,oldanchors=anchors;
    if(ep.ok&&(o.kind==1||o.kind==2)&&p.exists1) {int ar=anchor_object(fds[0],&p.st1);if(ar<0)ep.ok=0;else ep.anchor1=ar;}
    if(ep.ok&&o.kind==1&&p.exists2&&anchor_object(fds[1],&p.st2)<0)ep.ok=0;
    if(ep.ok&&!p.tmpfile) {
        if(o.kind==1) {
            int exchange=o.nr==SYS_renameat2&&(o.a[4]&RENAME_EXCHANGE);
            int structured=S_ISDIR(p.st1.st_mode)&&category(a)==category(b)&&!(o.nr==SYS_renameat2&&(o.a[4]&~(RENAME_NOREPLACE|RENAME_EXCHANGE)));
            if(structured) {
                if(p.exists2&&!exchange&&cp_object(b,fds[1],1,0))ep.ok=0;
                if(ep.ok)record_move(a,b,&p.st1,&p.st2,exchange);
            }
            else if(cp_object(b,fds[1],p.exists2,0)||cp_tree(fds[0],a,b))ep.ok=0;
            if(ep.ok&&!structured&&exchange&&cp_tree(fds[1],b,a))ep.ok=0;
        } else if(o.kind==3||o.kind==4){if(cp_object(b,fds[1],p.exists2,0))ep.ok=0;}
        else if(cp_object(a,fds[0],p.exists1,0))ep.ok=0;
    }
    if(!ep.ok){reply.error=-errno;denied++;}
    int changed=previous!=npaths||moves!=oldmoves||anchors!=oldanchors;
    if(changed)durable_barrier();
    if(generation>durable)durable_barrier();
    unsigned long seen=durable;
    if(audit){fprintf(audit,"{\"id\":%lu,\"registered\":%d,\"durable_generation\":%lu,\"execute_after\":%.9f}\n",(unsigned long)n.id,changed,seen,now());fflush(audit);}
    pthread_mutex_unlock(&ledger_mu);
    if(ep.ok)v3_rendezvous(a[0]?a:b,0);
    if(ep.ok&&ioctl(listener,SECCOMP_IOCTL_NOTIF_ID_VALID,&n.id)){ep.ok=0;reply.error=-ENOENT;}
    if(send(sock,&ep,sizeof(ep),MSG_NOSIGNAL)!=sizeof(ep)){reply.error=-EIO;ep.ok=0;}
    if(ep.ok) {
        struct outcome r;int resultfd=-1;int got=recv_packet(sock,&r,sizeof(r),&resultfd);__sync_fetch_and_add(&emulated,1);
        if(got<0){mark_uncertain("代执行 worker 未返回结果");reply.error=-EIO;}
        else {
            __sync_fetch_and_add(&checks,r.verified);__sync_fetch_and_add(&mismatches,r.mismatch);
            if(r.mismatch){
                fprintf(stderr,"核对不一致 nr=%d kind=%d expected_links=%lu actual_links=%lu source=%s target=%s\n",o.nr,o.kind,r.expected_links,r.actual_links,a,b);
                char file[4096];snprintf(file,sizeof(file),"%s/conflicts.jsonl",state);
                FILE *f=fopen(file,"a");jsonstr(f,a);fputc('\n',f);if(b[0]){jsonstr(f,b);fputc('\n',f);}fclose(f);
                mark_uncertain("目录项身份与前态不一致");
            }
            v3_rendezvous(a,1);
            if(r.value<0)reply.error=-r.err;
            else if(o.isopen&&got==1) {
                struct seccomp_notif_addfd ad={.id=n.id,.flags=SECCOMP_ADDFD_FLAG_SEND,.srcfd=resultfd,.newfd_flags=(o.flags&O_CLOEXEC)?O_CLOEXEC:0};
                if(ioctl(listener,SECCOMP_IOCTL_NOTIF_ADDFD,&ad)<0){__sync_fetch_and_add(&reply_failures,1);mark_uncertain("代执行后 ADDFD 失败");}
                close(resultfd);reply.id=0;
            }else reply.val=r.value;
        }
    }
unlock:
    for(int i=0;i<count;i++)close(fds[i]);
    if(l2>=0)pthread_mutex_unlock(&directory_locks[l2]);
    if(l1>=0)pthread_mutex_unlock(&directory_locks[l1]);
    pthread_rwlock_unlock(&workspace_lock);
    if(getenv("WI_TRACE")&&reply.error)fprintf(stderr,"错误 nr=%d errno=%d source=%s target=%s\n",o.nr,-reply.error,o.s,o.t);
    if(reply.id&&ioctl(listener,SECCOMP_IOCTL_NOTIF_SEND,&reply)&&!reply.error&&!reply.flags){__sync_fetch_and_add(&reply_failures,1);mark_uncertain("代执行后回复失败");}
}
static void *node_thread(void *arg) {
    int slot=(int)(intptr_t)arg;
    for(;;) {
        pthread_mutex_lock(&queue_mu);
        while(qread==qwrite&&!stopping)pthread_cond_wait(&queue_cv,&queue_mu);
        if(qread==qwrite&&stopping){pthread_mutex_unlock(&queue_mu);return NULL;}
        struct queued_request q=queue[qread++%4096];pthread_mutex_unlock(&queue_mu);
        notification(slot,q.notification,q.request);
    }
}
int main(int argc,char **argv) {
    if(argc<7)return 2;
    if(strcmp(argv[1],"B3"))return baseline_main(argc,argv);
    mode=argv[1];if(!realpath(argv[2],root)||!realpath(argv[3],state))die("目录");
    init_grants();
    char p[4096];snprintf(p,sizeof(p),"%s/ledger.jsonl",state);ledger=fopen(p,"w");if(!ledger)die("账本");
    journalfd=fileno(ledger);statefd=open(state,O_DIRECTORY|O_CLOEXEC);
    if(getenv("WI_AUDIT")){snprintf(p,sizeof(p),"%s/durability.jsonl",state);audit=fopen(p,"w");}
    if(getenv("WI_RACE_CONTROL")){int fd=open(getenv("WI_RACE_CONTROL"),O_RDWR|O_CLOEXEC);if(fd<0)die("竞态控制");race=mmap(NULL,4096,PROT_READ|PROT_WRITE,MAP_SHARED,fd,0);close(fd);}
    prctl(PR_SET_CHILD_SUBREAPER,1);struct sigaction sa={.sa_handler=timeout_handler};sigaction(SIGALRM,&sa,NULL);
    for(int i=0;i<3;i++){struct stat st;if(fstat(i,&st)||!S_ISFIFO(st.st_mode)){fprintf(stderr,"标准流必须是管道\n");return 125;}}
    int socks[2];if(socketpair(AF_UNIX,SOCK_SEQPACKET|SOCK_CLOEXEC,0,socks))die("启动 socket");
    double start=now();pid_t parent=getpid();launchpid=fork();if(launchpid<0)die("fork");
    if(!launchpid){close(socks[0]);fclose(ledger);close(statefd);v2_launch(socks[1],argv+6,parent);}
    close(socks[1]);alarm(atoi(argv[4]));pid_t init;
    if(recv(socks[0],&init,sizeof(init),0)!=sizeof(init))die("init pid");
    initpid=init;
    char ready;if(recv(socks[0],&ready,1,0)!=1)die("init ready");
    listener=recvfd(socks[0]);if(listener<0)die("listener");close(socks[0]);
    if(ioctl(listener,SECCOMP_IOCTL_NOTIF_SET_FLAGS,SECCOMP_USER_NOTIF_FD_SYNC_WAKE_UP))die("同步唤醒");
    snprintf(p,sizeof(p),"/proc/%d/root%s",initpid,state);
    anchorfd=open(p,O_PATH|O_DIRECTORY|O_CLOEXEC);if(anchorfd<0)die("锚点目录挂载视图");
    start_worker();
    snprintf(p,sizeof(p),"%s/pids.json",state);FILE *pf=fopen(p,"w");fprintf(pf,"{\"node\":%d,\"init\":%d,\"launcher\":%d,\"worker\":%d}\n",getpid(),initpid,launchpid,worker_pid);fclose(pf);
    pthread_t threads[V3_SLOTS];for(int i=0;i<257;i++)pthread_mutex_init(&directory_locks[i],NULL);
    for(int i=0;i<V3_SLOTS;i++)pthread_create(&threads[i],NULL,node_thread,(void*)(intptr_t)i);
    int pidfd=syscall(SYS_pidfd_open,launchpid,0);if(pidfd<0)die("pidfd");
    struct pollfd f[2]={{listener,POLLIN,0},{pidfd,POLLIN,0}};
    while(!timedout&&!uncertain) {
        int r=poll(f,2,-1);if(r<0){if(errno==EINTR)continue;die("poll");}if(f[1].revents)break;
        if(f[0].revents&POLLIN) {
            struct seccomp_notif n={0};if(ioctl(listener,SECCOMP_IOCTL_NOTIF_RECV,&n))continue;
            struct request request;char a[4096]="",b[4096]="";
            int route=copy_request(&n,&request)?-errno:fast_policy(n.pid,&request,a,b);
            if(route) {
                struct seccomp_notif_resp reply={.id=n.id};__sync_fetch_and_add(&calls,1);
                if(route>0){v3_rendezvous(a[0]?a:b,0);reply.flags=SECCOMP_USER_NOTIF_FLAG_CONTINUE;continued++;}
                else reply.error=route;
                ioctl(listener,SECCOMP_IOCTL_NOTIF_SEND,&reply);
                continue;
            }
            /* CONTINUE 在接收线程完成；代执行队列携带同一份参数副本，不再读取子进程参数。 */
            pthread_mutex_lock(&queue_mu);if(qwrite-qread>=4096)die("通知队列已满");
            queue[qwrite++%4096]=(struct queued_request){n,request};
            pthread_cond_signal(&queue_cv);pthread_mutex_unlock(&queue_mu);
        }
    }
    if(uncertain&&initpid>0)kill(initpid,SIGKILL);
    pthread_mutex_lock(&queue_mu);stopping=1;pthread_cond_broadcast(&queue_cv);pthread_mutex_unlock(&queue_mu);
    for(int i=0;i<V3_SLOTS;i++)pthread_join(threads[i],NULL);
    stop_flush_threads();
    for(int i=0;i<V3_SLOTS;i++)close(channels[i][0]);
    int st,code=0;while(waitpid(launchpid,&st,0)<0&&errno==EINTR){}
    while(waitpid(initpid,&st,0)<0&&errno==EINTR){}
    code=WIFEXITED(st)?WEXITSTATUS(st):128+WTERMSIG(st);while(waitpid(-1,&st,0)>0||errno==EINTR){}
    alarm(0);close(listener);close(pidfd);fclose(ledger);close(statefd);close(anchorfd);if(audit)fclose(audit);
    snprintf(p,sizeof(p),"%s/stats.json",state);FILE *out=fopen(p,"w");
    fprintf(out,"{\"mode\":\"B3\",\"wall_s\":%.9f,\"code\":%d,\"calls\":%lu,\"registered\":%zu,\"clones\":%lu,\"denied\":%lu,\"errors\":%lu,\"emulated\":%lu,\"continued\":%lu,\"workers\":%d,\"worker_threads\":%d,\"inode_checks\":%d,\"mismatches\":%d,\"reply_failures\":%d,\"indeterminate\":%d,\"timeout\":%d,\"race_handshakes\":%d,\"landlock_abi\":%d,\"lock_waits\":%lu,\"anchors\":%lu,\"moves\":%lu,\"batches\":%lu,\"max_batch\":%lu}\n",now()-start,code,calls,npaths,clones,denied,errors,emulated,continued,worker_count,V3_SLOTS,checks,mismatches,reply_failures,uncertain,timedout,race_done,ll_abi,lock_waits,anchors,moves,batches,max_batch);
    fclose(out);return timedout?124:uncertain?120:code;
}
