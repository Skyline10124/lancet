#!/usr/bin/env python3
"""审查补充：存在无关脏数据时，syncfs 与只对一个小文件 fsync 的耗时对比。"""
import ctypes
import json
import os
import sys
import time

libc = ctypes.CDLL(None, use_errno=True)


def main():
    if len(sys.argv) != 3:
        raise SystemExit(f'usage: {sys.argv[0]} SCRATCH_DIR OUTPUT_JSON')
    scratch, out = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
    os.makedirs(scratch, exist_ok=True)
    small = os.path.join(scratch, 'merged.txt')
    big = os.path.join(scratch, 'unrelated.bin')

    def timed(fn):
        start = time.monotonic()
        fn()
        return round((time.monotonic() - start) * 1e3, 1)

    def syncfs():
        fd = os.open(scratch, os.O_RDONLY)
        try:
            if libc.syncfs(fd) != 0:
                raise OSError(ctypes.get_errno(), 'syncfs')
        finally:
            os.close(fd)

    def fsync_small():
        fd = os.open(small, os.O_WRONLY | os.O_CREAT | os.O_TRUNC)
        try:
            os.write(fd, b'x' * 4096)
            os.fsync(fd)
        finally:
            os.close(fd)

    def dirty(gib):
        chunk = os.urandom(1 << 20)
        with open(big, 'wb') as f:
            for _ in range(gib * 1024):
                f.write(chunk)

    result = {'kernel': os.uname().release, 'scratch': scratch}
    try:
        syncfs()
        result['syncfs_quiet_ms'] = timed(syncfs)
        result['fsync_small_quiet_ms'] = timed(fsync_small)
        for gib in (1, 2):
            dirty(gib)
            result[f'syncfs_after_{gib}GiB_unrelated_ms'] = timed(syncfs)
            dirty(gib)
            result[f'fsync_small_after_{gib}GiB_unrelated_ms'] = timed(fsync_small)
            syncfs()
    finally:
        for path in (big, small):
            if os.path.exists(path):
                os.unlink(path)
    with open(out, 'w') as f:
        json.dump(result, f, indent=1)
        f.write('\n')
    print(json.dumps(result, indent=1))


if __name__ == '__main__':
    main()
