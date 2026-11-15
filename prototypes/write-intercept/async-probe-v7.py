"""After a writer that submitted one big O_DIRECT write is SIGKILLed and reaped, does the data keep landing?"""
import ctypes, json, mmap, os, subprocess, sys, time
libc = ctypes.CDLL(None, use_errno=True)
P = os.path.dirname(os.path.abspath(__file__))
base = os.path.abspath(sys.argv[1]); mib = int(sys.argv[2]); os.makedirs(base, exist_ok=True)

def run(mode, rep):
    path = os.path.join(base, f'{mode}-{rep}.bin'); size = mib << 20
    with open(path, 'wb') as f:
        chunk = b'\0' * (1 << 20)
        for _ in range(mib):
            f.write(chunk)
        f.flush(); os.fsync(f.fileno())
    ifd = libc.inotify_init1(os.O_NONBLOCK | os.O_CLOEXEC)
    libc.inotify_add_watch(ifd, base.encode(), 0x2 | 0x8)  # IN_MODIFY | IN_CLOSE_WRITE
    rfd = os.open(path, os.O_RDONLY | os.O_DIRECT)
    blk = mmap.mmap(-1, 4096)
    offsets = [i * size // 16 for i in range(16)] + [size - 4096]
    def landed():
        n = 0
        for off in offsets:
            os.preadv(rfd, [blk], off)
            n += blk[0] == 0xAB
        return n
    def drain():
        n = 0
        while True:
            try:
                n += len(os.read(ifd, 65536)) > 0
            except BlockingIOError:
                return n
    child = subprocess.Popen([f'{P}/.scratch/asyncwrite-v7', mode, path, str(mib)], stdout=subprocess.PIPE)
    child.stdout.readline(); t_sub = time.monotonic()
    child.wait(); t_reap = time.monotonic()
    at_reap = landed(); ev_at_reap = drain()
    timeline, ev_later = [[0.0, at_reap]], []
    while time.monotonic() - t_reap < 15 and timeline[-1][1] < len(offsets):
        n = landed()
        if n != timeline[-1][1]:
            timeline.append([round((time.monotonic() - t_reap) * 1e3, 1), n])
        if drain():
            ev_later.append(round((time.monotonic() - t_reap) * 1e3, 1))
        time.sleep(0.002)
    time.sleep(0.2)
    if drain():
        ev_later.append('late')
    full_ok=True
    with open(path,'rb') as f:
        while chunk:=f.read(1<<20):
            if chunk!=b'\xab'*len(chunk):full_ok=False
    os.close(rfd); os.close(ifd); os.unlink(path)
    return {'full_content_complete_after_reap':full_ok,'mode': mode, 'rep': rep, 'submit_to_reap_ms': round((t_reap - t_sub) * 1e3, 1),
            'samples': len(offsets), 'landed_at_reap': at_reap, 'landing_timeline_ms_after_reap': timeline,
            'inotify_batches_by_reap': ev_at_reap, 'inotify_after_reap_ms': ev_later}

print(json.dumps([run(m, r) for m in ('aio',) for r in (1, 2)], indent=1))
