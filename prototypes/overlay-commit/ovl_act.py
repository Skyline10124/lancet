#!/usr/bin/env python3
"""实验入口；真实路径挂载 overlay，退出命名空间后提交。"""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import traceback
import uuid

from commit import commit, fs_time, snapshot

BASE = Path.home() / 'exp/overlay'


def command(argv, cwd=None, env=None):
    t = time.monotonic()
    r = subprocess.run([str(x) for x in argv], cwd=cwd, env=env, capture_output=True, text=True, errors='replace')
    return dict(command=[str(x) for x in argv], code=r.returncode, stdout=r.stdout, stderr=r.stderr,
                seconds=time.monotonic()-t)


def save(path, data):
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    Path(path).write_text(json.dumps(data, ensure_ascii=False, indent=2) + '\n')


def git_state(repo):
    return dict(status=command(['git', '--no-optional-locks', '-C', repo, 'status', '--porcelain']),
                fsck=command(['git', '--no-optional-locks', '-C', repo, 'fsck']),
                head=command(['git', '-C', repo, 'rev-parse', 'HEAD']),
                branch=command(['git', '-C', repo, 'symbolic-ref', '--short', 'HEAD']))


def child(config_path):
    c = json.loads(Path(config_path).read_text())
    repo, state = Path(c['repo']), Path(c['state'])
    record = {'mount_commands': []}
    started = time.monotonic()
    def mount(*args):
        r = command(['mount', *args])
        record['mount_commands'].append(r)
        save(state / 'child.json', record)
        if r['code']:
            raise RuntimeError(r['stderr'])
    mount('--make-rprivate', '/')
    if c['git_policy'] != 'P1':
        (state / 'real-git').mkdir()
        mount('--bind', str(repo / '.git'), str(state / 'real-git'))
    mount('-t', 'overlay', 'overlay', '-o',
          f'lowerdir={repo},upperdir={state}/upper,workdir={state}/work,userxattr', str(repo))
    if c['git_policy'] != 'P1':
        mount('--bind', str(state / 'real-git'), str(repo / '.git'))
        if c['git_policy'] == 'P2':
            mount('-o', 'remount,bind,ro', str(repo / '.git'))
    record['mount_s'] = time.monotonic()-started
    record['mountinfo'] = Path('/proc/self/mountinfo').read_text()
    env = dict(os.environ, OVL_CONTROL=str(state))
    record['script'] = command(['bash', c['script']], cwd=repo, env=env)
    record['inside_git'] = git_state(repo)
    save(state / 'child.json', record)


def run(args):
    start = time.monotonic()
    repo = Path(args.repo).resolve()
    # 原型所有可写目录限定在实验区。
    if not repo.is_relative_to(BASE) or not (repo / '.git').is_dir():
        raise ValueError('仓库必须位于 ~/exp/overlay/ 且具有 .git 目录')
    run_id = args.run_id or uuid.uuid4().hex
    state = BASE / 'state' / run_id
    state.mkdir(parents=True)
    for name in ('upper', 'work'):
        (state / name).mkdir()
    result = dict(run_id=run_id, repo=str(repo), state=str(state), script=str(Path(args.script).resolve()),
                  script_text=Path(args.script).read_text(), git_policy=args.git_policy, check=args.check,
                  on_fail=args.on_fail, finish=args.finish,
                  start_utc=datetime.now(timezone.utc).isoformat(), free_bytes=shutil.disk_usage(repo).free)
    output = Path(args.output) if args.output else BASE / 'runs' / (run_id + '.json')
    if result['free_bytes'] < 15 * 1024**3:
        result.update(outcome='blocked', error='可用空间低于 15 GiB')
        save(output, result)
        return 2
    try:
        result['t0_ns'] = fs_time(state)
        baseline = None
        result['snapshot'] = {}
        if args.check in ('C1', 'both'):
            for include in (True, False):
                t = time.monotonic()
                snap = snapshot(repo, include)
                result['snapshot']['with_target' if include else 'without_target'] = dict(
                    seconds=time.monotonic()-t, entries=len(snap),
                    files=sum(v is not None and (v['mode'] & 0o170000) != 0o040000 for v in snap.values()))
                if not include:
                    baseline = snap
        save(state / 'config.json', result)
        ns = command(['unshare', '--user', '--map-root-user', '--mount',
                      sys.executable, Path(__file__).resolve(), '_child', str(state / 'config.json')])
        result['namespace'] = ns
        child_path = state / 'child.json'
        result['child'] = json.loads(child_path.read_text()) if child_path.exists() else {}
        if ns['code'] or 'script' not in result['child']:
            result['outcome'] = 'blocked'
        else:
            failed = result['child']['script']['code'] != 0
            commit_result = commit(repo, state / 'upper', state / 'preimage', baseline,
                                   result['t0_ns'], args.check,
                                   only_target=failed and args.on_fail == 'keep-regenerable',
                                   discard=args.finish == 'discard' or (failed and args.on_fail == 'discard'))
            result.update(commit_result)
            if failed:
                result['outcome'] = 'failed-kept-regenerable' if commit_result['outcome'] == 'committed' else 'failed-discarded'
            result['upper_counts'] = {g: dict(entries=0, bytes=0, files=0) for g in ('.git', 'target', 'other')}
            for e in result['classification']['entries']:
                group = e['path'].split('/')[0]
                group = group if group in ('.git', 'target') else 'other'
                result['upper_counts'][group]['entries'] += 1
                if (e['mode'] & 0o170000) == 0o100000:
                    result['upper_counts'][group]['bytes'] += e['size']
                    result['upper_counts'][group]['files'] += 1
        result['after_git'] = git_state(repo)
    except Exception:
        result.update(outcome='blocked', error=traceback.format_exc())
    result['total_s'] = time.monotonic()-start
    result['end_utc'] = datetime.now(timezone.utc).isoformat()
    save(output, result)
    print(json.dumps(dict(run_id=run_id, outcome=result['outcome'], output=str(output)), ensure_ascii=False), flush=True)
    return 2 if result['outcome'] == 'blocked' else 0


def main():
    if len(sys.argv) > 1 and sys.argv[1] == '_child':
        child(sys.argv[2])
        return 0
    p = argparse.ArgumentParser()
    p.add_argument('action', choices=['run'])
    p.add_argument('--repo', required=True)
    p.add_argument('--script', required=True)
    p.add_argument('--git-policy', choices=['P1', 'P2', 'P3'], required=True)
    p.add_argument('--check', choices=['C1', 'C4', 'both'], required=True)
    p.add_argument('--on-fail', choices=['discard', 'keep-regenerable'], default='discard')
    p.add_argument('--finish', choices=['commit', 'discard'], default='commit')
    p.add_argument('--run-id')
    p.add_argument('--output')
    return run(p.parse_args())


if __name__ == '__main__':
    sys.exit(main())
