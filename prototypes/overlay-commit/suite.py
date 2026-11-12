#!/usr/bin/env python3
"""场景驱动；每个可重复单元三次，基础设施连续失败两次即记阻塞。"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import time
import traceback

from ovl_act import BASE, command, git_state, save

HERE = Path(__file__).resolve().parent
REPO = BASE / 'repo'
RESULTS = HERE / 'results'
FILES = ['README.md', 'LICENSE', 'AGENTS.md']
CRATE = 'net-guard'
DIR = 'crates/net-guard'
SOURCE = DIR + '/src/lib.rs'
BASELINE = BASE / 'baseline'


def checked(argv, cwd=None):
    r = command(argv, cwd)
    if r['code']:
        raise RuntimeError(json.dumps(r, ensure_ascii=False))
    return r


def prepare():
    if BASELINE.exists():
        return
    checked(['git', '-C', REPO, 'config', 'user.name', 'lattice-devbot[bot]'])
    checked(['git', '-C', REPO, 'config', 'user.email', '325403158+lattice-devbot[bot]@users.noreply.github.com'])
    checked(['git', '-C', REPO, 'config', 'commit.gpgsign', 'false'])
    checked(['git', '-C', REPO, 'remote', 'set-url', 'origin', 'https://github.com/Skyline10124/lattice.git'])
    checked(['cp', '-a', '--reflink=always', REPO, BASELINE])
    save(RESULTS / 'baseline.json', dict(head=command(['git', '-C', REPO, 'rev-parse', 'HEAD']),
                                        git=git_state(REPO), target=command(['du', '-sb', REPO / 'target'])))


def reset(target=False):
    # 这里只操作一次克隆出来的实验仓库，恢复 Git 元数据避免前一次 S6 改变下一次基线。
    assert REPO == BASE / 'repo'
    shutil.rmtree(REPO / '.git')
    checked(['cp', '-a', '--reflink=always', BASELINE / '.git', REPO / '.git'])
    checked(['git', '-C', REPO, 'reset', '--hard', 'HEAD'])
    checked(['git', '-C', REPO, 'clean', '-ffdx', '-e', 'target/'])
    if target:
        shutil.rmtree(REPO / 'target')
        checked(['cp', '-a', '--reflink=always', BASELINE / 'target', REPO / 'target'])


def barrier():
    return 'touch "$OVL_CONTROL/ready"\nwhile [ ! -e "$OVL_CONTROL/go" ]; do sleep 0.02; done\n'


def edit_script():
    return 'set -e\n' + '\n'.join('sed -i "1s/^/OVL /" ' + shlex.quote(p) for p in FILES) + '\n'


def start_run(scenario, variant, rep, script, policy='P1', on_fail='discard', finish='commit'):
    run_id = f'{scenario}-{variant}-{policy}-{rep}-{time.time_ns()}'
    path = BASE / 'scripts' / (run_id + '.sh')
    path.parent.mkdir(exist_ok=True)
    path.write_text(script)
    output = RESULTS / (run_id + '.json')
    args = [str(HERE / 'ovl-act'), 'run', '--repo', str(REPO), '--script', str(path),
            '--git-policy', policy, '--check', 'both', '--on-fail', on_fail,
            '--finish', finish, '--run-id', run_id, '--output', str(output)]
    process = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    return dict(process=process, output=output, state=BASE / 'state' / run_id, scenario=scenario,
                variant=variant, repetition=rep, args=args)


def wait_ready(run):
    deadline = time.monotonic() + 60
    while not (run['state'] / 'ready').exists():
        if run['process'].poll() is not None or time.monotonic() > deadline:
            raise RuntimeError('运行未进入同步点：' + str(run['state']))
        time.sleep(0.02)


def release(run):
    (run['state'] / 'go').touch()


def finish_run(run, extra=None):
    stdout, stderr = run['process'].communicate(timeout=600)
    result = json.loads(run['output'].read_text())
    result.update(scenario=run['scenario'], variant=run['variant'], repetition=run['repetition'],
                  invocation=dict(command=run['args'], code=run['process'].returncode, stdout=stdout, stderr=stderr))
    result.update(extra or {})
    save(run['output'], result)
    print(result['run_id'], result['outcome'], result.get('conflicts'), flush=True)
    return result


def assess(result, expected, observed, checks):
    result['expected'] = expected
    result['observed'] = observed
    result['assertions'] = checks
    result['matches_expectation'] = all(checks.values())
    truth = set(result.get('truth_conflict_paths', []))
    result['confusion'] = {c: dict(false_positive=sorted(set(paths)-truth), false_negative=sorted(truth-set(paths)))
                           for c, paths in result.get('conflicts', {}).items()}
    save(RESULTS / (result['run_id'] + '.json'), result)


def read_files(paths):
    return {p: (REPO / p).read_text() if (REPO / p).is_file() else None for p in paths}


def simple(scenario, rep, policy='P1', noise=None):
    reset()
    before = read_files(FILES + ['Cargo.toml'])
    use_barrier = scenario in ('S2', 'S3', 'S4')
    run = start_run(scenario, 'noise' if noise else 'default', rep,
                    edit_script() + (barrier() if use_barrier else ''), policy)
    external = {}
    noise_proc = None
    if use_barrier:
        wait_ready(run)
        if scenario in ('S3', 'S4'):
            p = FILES[0] if scenario == 'S3' else 'Cargo.toml'
            with (REPO / p).open('a') as f:
                f.write('\n# E-external\n')
            external[p] = (REPO / p).read_text()
        if noise:
            noise_proc = start_noise(noise, run['state'])
            time.sleep(noise['duration_s'])
        release(run)
    result = finish_run(run, dict(external=external, truth_conflict_paths=[FILES[0]] if scenario == 'S3' else []))
    if noise_proc:
        result['noise'] = stop_noise(noise_proc, run['state'])
    after = read_files(FILES + ['Cargo.toml'])
    if scenario == 'S3':
        checks = dict(conflict=result['outcome'] == 'conflict', external_preserved=after[FILES[0]] == external[FILES[0]],
                      batch_unchanged=all(after[p] == before[p] for p in FILES[1:]))
    else:
        checks = dict(committed=result['outcome'] == 'committed',
                      content=all(after[p] == 'OVL ' + before[p] for p in FILES),
                      no_residue=result.get('overlay_xattr_residual_count') == 0,
                      no_conflict=all(not p for p in result.get('conflicts', {}).values()),
                      external_preserved=all(after[p] == v for p, v in external.items()))
    assess(result, '冲突且整批不提交' if scenario == 'S3' else '提交正确，无冲突，无扩展属性残留', after, checks)


def s5(rep, mode):
    reset(target=True)
    text = (REPO / SOURCE).read_text()
    old = 'assert_eq!(url.len(), MAX_BASE_URL_BYTES);'
    assert text.count(old) == 1
    script = "set -e\nsed -i 's/assert_eq!(url.len(), MAX_BASE_URL_BYTES);/assert_eq!(url.len(), MAX_BASE_URL_BYTES + 1);/' " + SOURCE + '\ncargo build -p net-guard\ncargo test -p net-guard\n'
    run = start_run('S5', mode, rep, script, on_fail=mode)
    result = finish_run(run)
    source_after = (REPO / SOURCE).read_text()
    # 下一次构建保持同一个真实路径；失败时的源码必须已丢弃。
    result['next_build'] = command(['cargo', 'build', '-p', CRATE], cwd=REPO)
    assess(result, '测试失败、源码丢弃；记录下一次单 crate 构建', {'source_sha256': hashlib.sha256(source_after.encode()).hexdigest()},
           dict(test_failed=result.get('child', {}).get('script', {}).get('code') == 101,
                intended_failure='base_url' in result.get('child', {}).get('script', {}).get('stdout', ''),
                source_restored=source_after == text, next_build_ok=result['next_build']['code'] == 0))


def s7(rep, variant):
    reset()
    commands = {'mv': 'mv crates/net-guard crates/net-guard2',
                'git-mv': 'git mv crates/net-guard crates/net-guard2',
                'os-rename': "python3 -c \"import os; os.rename('crates/net-guard','crates/net-guard2')\""}
    result = finish_run(start_run('S7', variant, rep, commands[variant]+'\n'))
    script = result.get('child', {}).get('script', {})
    exdev = 'cross-device' in script.get('stderr', '').lower() or 'EXDEV' in script.get('stderr', '')
    assess(result, '逐项记录成功或 EXDEV', {'old_exists': (REPO / DIR).exists(), 'new_exists': (REPO / (DIR+'2')).exists(), 'exdev': exdev},
           dict(observed=script.get('code') == 0 or exdev, fsck_ok=result['after_git']['fsck']['code'] == 0))


def s8(rep):
    reset()
    script = 'set -e\nrm -rf crates/net-guard\nmkdir crates/net-guard\nprintf "new\\n" > crates/net-guard/new.txt\n'
    result = finish_run(start_run('S8', 'opaque', rep, script))
    children = sorted(p.name for p in (REPO / DIR).iterdir())
    assess(result, '下层同名目录仅含新文件', children,
           dict(committed=result['outcome'] == 'committed', children=children == ['new.txt'],
                no_residue=result.get('overlay_xattr_residual_count') == 0))


def s9(rep, variant):
    reset()
    paths = [FILES[0], FILES[1] if variant == 'different' else FILES[0]]
    before = read_files(paths)
    runs = [start_run('S9', variant+'-'+label, rep,
                      'set -e\nsed -i "1s/^/'+label+' /" '+shlex.quote(p)+'\n'+barrier())
            for p, label in zip(paths, ['A','B'])]
    for run in runs:
        wait_ready(run)
    # 两个 overlay 已重叠运行并各有草稿；明确 A 先提交，B 后预检。
    release(runs[0])
    a = finish_run(runs[0])
    release(runs[1])
    b = finish_run(runs[1], dict(truth_conflict_paths=[paths[0]] if variant == 'same' else []))
    final = read_files(paths)
    expected = {paths[0]: 'A '+before[paths[0]]}
    if variant == 'different':
        expected[paths[1]] = 'B '+before[paths[1]]
    for result in (a,b):
        result['pair'] = [a['run_id'], b['run_id']]
        result['commit_order'] = ['A', 'B']
        result['lost_changes'] = ['B（冲突拒绝）'] if variant == 'same' else []
        assess(result, '不同路径两次提交；同路径 A 提交、B 检出冲突', final,
               dict(final_content=final == expected,
                    outcome=result['outcome'] == ('conflict' if result is b and variant == 'same' else 'committed')))


def start_noise(config, state):
    return subprocess.Popen([str(HERE / 'ide-noise.sh'), '--repo', str(REPO), '--config', str(config['path']),
                             '--stop', str(state / 'noise-stop'), '--output', str(state / 'noise.json')],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def stop_noise(proc, state):
    (state / 'noise-stop').touch()
    stdout, stderr = proc.communicate(timeout=300)
    out = json.loads((state / 'noise.json').read_text())
    out.update(process_code=proc.returncode, process_stdout=stdout, process_stderr=stderr)
    return out


def s6(rep, policy, disposition, noise=None):
    reset()
    before_head = command(['git', '-C', REPO, 'rev-parse', 'HEAD'])['stdout']
    with (REPO / FILES[0]).open('a') as f:
        f.write('\nS6 input\n')
    before = git_state(REPO)
    script = 'set -e\n' + barrier() + 'git add -A && bot-run git commit -m t\ngit checkout -b t2\ngit stash\n'
    run = start_run('S6', ('noise' if noise else 'quiet')+'-'+disposition, rep, script, policy, finish=disposition)
    wait_ready(run)
    proc = start_noise(noise, run['state']) if noise else None
    if noise:
        time.sleep(noise['duration_s'])
    release(run)
    result = finish_run(run)
    if proc:
        result['noise'] = stop_noise(proc, run['state'])
    result['before_git'] = before
    after = result['after_git']
    script_ok = result.get('child', {}).get('script', {}).get('code') == 0
    script_error = result.get('child', {}).get('script', {}).get('stderr', '')
    if 'bot-run:' in script_error and not script_ok:
        result['outcome'] = 'blocked'
        result['error'] = script_error
    expected_head_changed = policy != 'P2' and disposition == 'commit'
    checks = dict(script=(not script_ok if policy == 'P2' else script_ok),
                  fsck=after['fsck']['code'] == 0,
                  head=(after['head']['stdout'] != before_head) == expected_head_changed)
    if policy == 'P2' or disposition == 'discard':
        checks['status_restored'] = after['status']['stdout'] == before['status']['stdout']
        checks['branch_restored'] = after['branch']['stdout'] == before['branch']['stdout']
    else:
        checks['status_clean'] = after['status']['stdout'] == ''
        checks['branch_t2'] = after['branch']['stdout'].strip() == 't2'
    assess(result, 'P2 写 Git 失败；提交一致；丢弃回到脚本前 Git 状态',
           {'script_ok':script_ok,'head_changed':after['head']['stdout'] != before_head,
            'index_conflict':{c:'.git/index' in v for c,v in result.get('conflicts',{}).items()}}, checks)


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--scenarios', nargs='+', default=['S1','S3','S4','S5','S7','S8','S9'])
    p.add_argument('--noise-config', type=Path)
    p.add_argument('--repetitions', type=int, default=3)
    args = p.parse_args()
    prepare()
    noise = None
    if args.noise_config:
        noise = json.loads(args.noise_config.read_text())
        noise['path'] = str(args.noise_config.resolve())
    jobs = []
    for scenario in args.scenarios:
        if scenario == 'S6':
            assert noise is not None
            for n in (None, noise):
                for policy in ('P1', 'P2', 'P3'):
                    for disposition in ('commit', 'discard'):
                        for rep in range(1, args.repetitions+1):
                            jobs.append((scenario, policy+'-'+disposition+'-'+str(bool(n)), s6,
                                         (rep, policy, disposition, n)))
            continue
        for rep in range(1, args.repetitions+1):
            if scenario in ('S1', 'S3', 'S4'):
                jobs.append((scenario, 'default', simple, (scenario, rep)))
            elif scenario == 'S2':
                assert noise is not None
                for policy in ('P1', 'P2', 'P3'):
                    jobs.append((scenario, policy, simple, (scenario, rep, policy, noise)))
            elif scenario == 'S5':
                for mode in ('discard', 'keep-regenerable'):
                    jobs.append((scenario, mode, s5, (rep, mode)))
            elif scenario == 'S7':
                for variant in ('mv', 'git-mv', 'os-rename'):
                    jobs.append((scenario, variant, s7, (rep, variant)))
            elif scenario == 'S8':
                jobs.append((scenario, 'opaque', s8, (rep,)))
            elif scenario == 'S9':
                for variant in ('different', 'same'):
                    jobs.append((scenario, variant, s9, (rep, variant)))
    failures = {}
    for scenario, variant, fn, params in jobs:
        key = scenario+'-'+variant
        if failures.get(key, 0) >= 2:
            save(RESULTS / ('blocked-skip-'+key+'-'+str(time.time_ns())+'.json'),
                 dict(step=key, reason='连续两次基础设施失败，跳过后续重复', params=str(params)))
            continue
        free = shutil.disk_usage(BASE).free
        save(RESULTS / (scenario+'-disk-'+str(time.time_ns())+'.json'),
             dict(scenario_guard=scenario, variant=variant, free_bytes=free, utc_ns=time.time_ns()))
        if free < 15*1024**3:
            raise RuntimeError('场景前可用空间低于 15 GiB')
        old_files = set(RESULTS.glob('S*.json'))
        try:
            fn(*params)
            produced = [json.loads(p.read_text()) for p in set(RESULTS.glob('S*.json'))-old_files]
            failed = any(r.get('outcome') == 'blocked' for r in produced)
            failures[key] = failures.get(key, 0)+1 if failed else 0
        except Exception:
            failures[key] = failures.get(key, 0)+1
            save(RESULTS / ('blocked-attempt-'+key+'-'+str(time.time_ns())+'.json'),
                 dict(step=key, command=sys.argv, params=str(params), error=traceback.format_exc(),
                      consecutive_failures=failures[key]))
            print('执行异常：'+key, traceback.format_exc(), flush=True)


if __name__ == '__main__':
    main()
