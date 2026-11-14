/* 第三轮私有状态；只用于本实验，不是产品接口。 */
#define V3_SLOTS 8
struct execute_permission { int ok, anchor1; };
static int hostproc=-1, worker_root=-1;
static struct __user_cap_data_struct worker_caps[2];
static void restore_worker_root(void) {
    struct __user_cap_header_struct h={_LINUX_CAPABILITY_VERSION_3,0};
    if(syscall(SYS_capset,&h,worker_caps)||fchdir(worker_root)||syscall(SYS_chroot,"."))die("恢复 worker 上下文");
}
struct writable_root {char path[4096];dev_t dev;ino_t ino;int directory;};
static struct writable_root grants[16];
static int ngrants,ll_abi;
static unsigned long continued, lock_waits, batches, max_batch, anchors, moves;
static int pending[200000],npending;
static pthread_mutex_t ledger_mu=PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t durable_cv=PTHREAD_COND_INITIALIZER;
static unsigned long generation,durable;
static int flush_active;
static FILE *audit;
static void pending_fd(int fd){if(npending==200000)die("前态队列已满");pending[npending++]=fd;}
/* 仅在出现多前态批次时建立常驻刷盘线程，多个 inode 的 fsync 同时进入 XFS。 */
static pthread_mutex_t flush_mu=PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t flush_cv=PTHREAD_COND_INITIALIZER;
static pthread_t flush_threads[V3_SLOTS];
static int flush_started,flush_stop,flush_next,flush_count,flush_done,flush_error;
static int *flush_fds;
static void *flush_thread(void *unused) {
    (void)unused;
    for(;;) {
        pthread_mutex_lock(&flush_mu);
        while(flush_next==flush_count&&!flush_stop)pthread_cond_wait(&flush_cv,&flush_mu);
        if(flush_stop){pthread_mutex_unlock(&flush_mu);return NULL;}
        int fd=flush_fds[flush_next++];pthread_mutex_unlock(&flush_mu);
        double start=now();int rc=fsync(fd),error=errno;double end=now();close(fd);
        pthread_mutex_lock(&flush_mu);
        if(rc)flush_error=error;
        if(audit){fprintf(audit,"{\"kind\":\"fsync\",\"start\":%.9f,\"end\":%.9f,\"errno\":%d}\n",start,end,rc?error:0);fflush(audit);}
        flush_done++;pthread_cond_broadcast(&flush_cv);pthread_mutex_unlock(&flush_mu);
    }
}
static void flush_clones(void) {
    if(npending<=1){if(npending){if(fsync(pending[0]))die("前态 fsync");close(pending[0]);}npending=0;return;}
    if(!flush_started){for(int i=0;i<V3_SLOTS;i++)pthread_create(&flush_threads[i],NULL,flush_thread,NULL);flush_started=1;}
    pthread_mutex_lock(&flush_mu);flush_fds=pending;flush_next=0;flush_done=0;flush_count=npending;flush_error=0;
    pthread_cond_broadcast(&flush_cv);
    while(flush_done<flush_count)pthread_cond_wait(&flush_cv,&flush_mu);
    if(flush_error){errno=flush_error;die("并发前态 fsync");}
    pthread_mutex_unlock(&flush_mu);npending=0;
}
static void stop_flush_threads(void) {
    if(!flush_started)return;
    pthread_mutex_lock(&flush_mu);flush_stop=1;pthread_cond_broadcast(&flush_cv);pthread_mutex_unlock(&flush_mu);
    for(int i=0;i<V3_SLOTS;i++)pthread_join(flush_threads[i],NULL);
}
static void add_grant(const char *path) {
    if(!path||!path[0])return;
    struct stat st;if(stat(path,&st))return;
    for(int i=0;i<ngrants;i++)if(!strcmp(grants[i].path,path))return;
    if(ngrants==16)die("Landlock 规则已满");
    struct writable_root *g=&grants[ngrants++];
    if(!realpath(path,g->path))die("Landlock 路径");
    g->dev=st.st_dev;g->ino=st.st_ino;g->directory=S_ISDIR(st.st_mode);
}
static void init_grants(void) {
    char p[4096];snprintf(p,sizeof(p),"%s/target",root);add_grant(p);
    snprintf(p,sizeof(p),"%s/.git",root);add_grant(p);
    add_grant(getenv("TMPDIR"));add_grant(getenv("CARGO_HOME"));
    add_grant(getenv("WI_CACHE"));add_grant(getenv("WI_RACE_CONTROL"));
    add_grant("/dev/null");add_grant("/dev/zero");add_grant("/dev/random");add_grant("/dev/urandom");
    ll_abi=syscall(SYS_landlock_create_ruleset,NULL,0,LANDLOCK_CREATE_RULESET_VERSION);
    if(ll_abi<3)die("需要 Landlock ABI 3");
}
static void install_landlock(void) {
    const uint64_t rights=LANDLOCK_ACCESS_FS_WRITE_FILE|LANDLOCK_ACCESS_FS_REMOVE_DIR|
      LANDLOCK_ACCESS_FS_REMOVE_FILE|LANDLOCK_ACCESS_FS_MAKE_CHAR|LANDLOCK_ACCESS_FS_MAKE_DIR|
      LANDLOCK_ACCESS_FS_MAKE_REG|LANDLOCK_ACCESS_FS_MAKE_SOCK|LANDLOCK_ACCESS_FS_MAKE_FIFO|
      LANDLOCK_ACCESS_FS_MAKE_BLOCK|LANDLOCK_ACCESS_FS_MAKE_SYM|LANDLOCK_ACCESS_FS_REFER|LANDLOCK_ACCESS_FS_TRUNCATE;
    struct landlock_ruleset_attr attr={.handled_access_fs=rights};
    int fd=syscall(SYS_landlock_create_ruleset,&attr,sizeof(attr),0);if(fd<0)die("Landlock ruleset");
    for(int i=0;i<ngrants;i++) {
        int p=open(grants[i].path,O_PATH|O_CLOEXEC);if(p<0)die("Landlock grant");
        struct landlock_path_beneath_attr a={.parent_fd=p,.allowed_access=grants[i].directory?rights:LANDLOCK_ACCESS_FS_WRITE_FILE|LANDLOCK_ACCESS_FS_TRUNCATE};
        if(syscall(SYS_landlock_add_rule,fd,LANDLOCK_RULE_PATH_BENEATH,&a,0))die("Landlock add rule");
        close(p);
    }
    if(prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0)||syscall(SYS_landlock_restrict_self,fd,0))die("Landlock restrict");
    close(fd);
}
/* 调用者持 ledger_mu：同代克隆、账本、锚点一同提交；写调用等待自己的代号。 */
static void durable_barrier(void) {
    unsigned long mine=++generation;
    if(flush_active){while(durable<mine)pthread_cond_wait(&durable_cv,&ledger_mu);return;}
    flush_active=1;
    while(durable<generation) {
        struct timespec until;clock_gettime(CLOCK_REALTIME,&until);until.tv_nsec+=200000;
        if(until.tv_nsec>=1000000000){until.tv_sec++;until.tv_nsec-=1000000000;}
        pthread_cond_timedwait(&durable_cv,&ledger_mu,&until);
        unsigned long upto=generation,size=upto-durable;
        /* 锁保护提交边界；fsync 只在收齐本代之后开始，绝不提前执行写调用。 */
        flush_clones();
        if(fsync(journalfd)||fsync(statefd))die("账本 fsync");
        if(audit){fprintf(audit,"{\"kind\":\"commit\",\"upto\":%lu,\"size\":%lu,\"complete\":%.9f}\n",upto,size,now());fflush(audit);}
        batches++;if(size>max_batch)max_batch=size;durable=upto;pthread_cond_broadcast(&durable_cv);
    }
    flush_active=0;
}
