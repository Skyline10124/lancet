"""冻结的任务判定：文件精确比较、返回事实、调用时序。"""
import argparse
import json
from pathlib import Path
import re
from commands import normalize


def files_at(root):
    return {str(p.relative_to(root)): p.read_text() for p in sorted(Path(root).rglob('*')) if p.is_file()}


def leaves(value):
    yield value
    if isinstance(value, dict):
        for v in value.values():
            yield from leaves(v)
    elif isinstance(value, list):
        for v in value:
            yield from leaves(v)


def has_count(value, name, count):
    """接受文件→整数、文件→计数对象、记录列表及 wc 原始行；不跨文件配数。"""
    def same_file(v):
        return isinstance(v, str) and v in (name, 'data/' + name)

    def number(v):
        return type(v) is int and v == count

    def count_field(v):
        return isinstance(v, dict) and any(
            k in ('lines', 'count', 'line_count', 'lineCount') and number(n)
            for k, n in v.items())

    for item in leaves(value):
        if isinstance(item, dict):
            if any(same_file(k) and (number(v) or count_field(v)) for k, v in item.items()):
                return True
            if any(same_file(item.get(k)) for k in ('path', 'file', 'name', 'filename')) and count_field(item):
                return True
        if isinstance(item, str):
            path = r'(?:data/)?' + re.escape(name)
            if re.search(r'(?m)^\s*' + str(count) + r'\s+' + path + r'\s*$', item):
                return True
            if re.search(path + r'["\\\s:=>,()\[\]-]{0,20}' + str(count) + r'\b', item):
                return True
            try:
                decoded = json.loads(item)
            except ValueError:
                continue
            if isinstance(decoded, (dict, list)) and has_count(decoded, name, count):
                return True
    return False


def evaluate(task, files, calls, final=''):
    spec = json.loads((task / 'spec.json').read_text())
    category = spec['category']
    reasons = []
    if files != spec['expected']:
        reasons.append('最终文件不符合期望')
    values = [c['outcome']['value'] for c in calls if c['outcome']['ran']]
    evidence = '\n'.join(json.dumps(v, ensure_ascii=False, sort_keys=True) for v in values)
    # 草稿明确允许 fail 回滚后在最终回复中汇报。
    if category == 'rollback':
        evidence += '\n' + final
    for fact in spec['facts']:
        pattern = r'(?<!\d)' + re.escape(fact) + r'(?!\d)' if fact.isdigit() else re.escape(fact)
        if not re.search(pattern, evidence, re.I):
            reasons.append('缺少事实: ' + fact)
    events = [(i,e) for i,c in enumerate(calls) for e in c['outcome'].get('events',[])]
    if category == 'edit':
        path = spec['replacement'][0]
        reads = [i for i,e in events if e['op']=='read' and e['args']['path']==path]
        writes = [i for i,e in events if e['op'] in ('edit','write') and e['args']['path']==path]
        if not reads or not writes or min(reads) >= min(writes):
            reasons.append('未在较早一次 act 读取目标后再修改')
    if spec['command']:
        runs = [e for _,e in events if e['op']=='run' and normalize(e['args']['argv'])==normalize(spec['command'])]
        if not runs:
            reasons.append('未执行指定测试命令')
        if category == 'rollback':
            path,old,new = spec['replacement']
            candidate = spec['expected'][path].replace(old,new)
            if not any(e['args'].get('files',{}).get(path)==candidate for e in runs):
                reasons.append('未在要求的修改上运行测试')
    if spec['id']=='read-03':
        for name,count in [('a.txt',3),('b.txt',4)]:
            if not any(has_count(v, name, count) for v in values):
                reasons.append('行数未与文件对应: '+name)
    if spec['id'] in ('probe-02','probe-04'):
        wanted = ['alpha','zeta'] if spec['id']=='probe-02' else ['east','west']
        objects = [v for v in values if isinstance(v,dict)]
        # 也接受 JSON 字符串返回；仍校验结构与顺序。
        for v in values:
            if isinstance(v,str):
                try:
                    obj=json.loads(v)
                    if isinstance(obj,dict): objects.append(obj)
                except ValueError:
                    pass
        structured = [x for obj in objects for x in leaves(obj)]
        number = 16300 if spec['id']=='probe-02' else 5
        if wanted not in structured or not any(type(x) is int and x==number for x in structured):
            reasons.append('结构化返回的排序或合计不符')
        if spec['id']=='probe-04' and not any(x is True for x in structured):
            reasons.append('未返回存在性布尔值 true')
    return dict(passed=not reasons, reasons=reasons)


def cli(task):
    parser=argparse.ArgumentParser()
    parser.add_argument('record',type=Path)
    args=parser.parse_args()
    record=json.loads(args.record.read_text())
    print(json.dumps(evaluate(task,record['files'],record['calls'],record.get('final','')),ensure_ascii=False))
