#!/usr/bin/env python3
"""只读被动记录；冻结校准输入并按两个覆盖区间统计。"""
import argparse
from collections import Counter
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import time
from ovl_act import BASE, command, save

HERE = Path(__file__).resolve().parent


def category(row):
    p = row.get('path', '')
    if row.get('kind') == 'overflow':
        return 'overflow'
    if row.get('kind'):
        return 'metadata'
    if p == '.git/index':
        return 'git_index'
    if p.startswith('.git/'):
        return 'git_other'
    if p.split('/')[0] in ('.codex', '.aws'):
        return 'other_agent_sandbox'
    return 'source_other'


def utc(t):
    return datetime.fromtimestamp(t, timezone.utc).isoformat()


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--output', type=Path, default=HERE / 'noise-config.json')
    p.add_argument('--duration', type=float, default=30)
    args = p.parse_args()
    end = time.time()
    saved = HERE / 'results' / ('calibration-' + str(time.time_ns()))
    saved.mkdir()
    sources = {}
    for name in ('events.jsonl', 'target_per_minute.jsonl'):
        original = BASE / 'lattice-writes' / name
        content = original.read_bytes()
        # 活跃记录器可能正在写最后一行；只冻结完整行。
        complete = content[:content.rfind(b'\n')+1]
        (saved / name).write_bytes(complete)
        sources[name] = dict(path=str(original), bytes=len(complete), sha256=hashlib.sha256(complete).hexdigest(),
                             truncated_tail_bytes=len(content)-len(complete))
    events = [json.loads(l) for l in (saved / 'events.jsonl').read_text().splitlines()]
    targets = [json.loads(l) for l in (saved / 'target_per_minute.jsonl').read_text().splitlines()]
    starts = [r['t'] for r in events if r.get('kind') == 'start']
    # 第一段终止时间没有 end 事件；以最后一条事件作为已观测下界。
    first_end = max(r['t'] for r in events if r['t'] < starts[1])
    segments = []
    for a, b in ((starts[0], first_end), (starts[1], end)):
        rows = [r for r in events if a <= r['t'] <= b]
        counts = Counter(category(r) for r in rows)
        path_counts = Counter(r.get('path') for r in rows if r.get('path'))
        evcounts = Counter(ev for r in rows for ev in r.get('ev', []))
        segments.append(dict(start_utc=utc(a), end_utc=utc(b), duration_s=b-a, events=len(rows),
                             counts=dict(counts), events_per_minute={k:v*60/(b-a) for k,v in counts.items()},
                             paths=dict(path_counts), event_kinds=dict(evcounts),
                             target_records=[r for r in targets if a<=r.get('t',r.get('minute',0))<=b]))
    duration = sum(s['duration_s'] for s in segments)
    def count_path(test, ev):
        return sum(test(r.get('path','')) and ev in r.get('ev',[]) for r in events)
    starts_count = {
        'index_refresh': count_path(lambda p:p=='.git/index.lock','create'),
        'fetch_head':count_path(lambda p:p=='.git/FETCH_HEAD','close_write'),
        'ephemeral_dirs':count_path(lambda p:p=='.codex','create'),
        'checkpoint':count_path(lambda p:p.startswith('.git/t3-checkpoint-index-') and not p.endswith('.lock'),'create'),
        'config_lock':count_path(lambda p:p=='.git/config.lock','create'),
        'packed_refs_lock':count_path(lambda p:p=='.git/packed-refs.lock','create'),
        'remote_head_lock':count_path(lambda p:p=='.git/refs/remotes/origin/HEAD.lock','create'),
        'worktree_index':count_path(lambda p:p.startswith('.git/worktrees/') and p.endswith('/index.lock'),'create'),
        'source_touch':count_path(lambda p:not p.startswith('.') and p!='','attrib'),
    }
    config = dict(duration_s=args.duration, git_status_interval_s=1.0, cargo_check_once=True,
                  initial_phase='seeded-uniform', ephemeral_lifetime_s=0.1, seed=20261112,
                  intervals_s={k:duration/n if n else None for k,n in starts_count.items()},
                  source=str(saved.relative_to(HERE)), observed_operations=starts_count, covered_seconds=duration,
                  coverage=segments, sources=sources,
                  other_agent_sandbox_attribution='维护者核实：同一仓库另一个 Codex 会话的 bwrap 临时只读 tmpfs 挂载点',
                  unobservable=['git status 无写入时不进入 inotify 记录；保留方案 1 秒轮询',
                                'target 空记录无法区分无写入与子目录监控盲区；保留启动一次 cargo check'],
                  baseline=dict(git_status_interval_s=1.0,source_touch_interval_s=5.0,cargo_check_once=True))
    save(args.output,config)
    save(saved / 'calibration.json',config)
    save(saved / 'recorder-status.json',command(['systemctl','--user','status','lattice-write-recorder','--no-pager']))
    print(json.dumps({k:config[k] for k in ('source','covered_seconds','intervals_s','observed_operations')},indent=2))


if __name__ == '__main__':
    main()
