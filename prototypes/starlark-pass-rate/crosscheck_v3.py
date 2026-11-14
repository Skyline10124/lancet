"""独立用 SQLite 对原始 JSONL 重算主要分子分母，核对报告聚合。"""
import json
from pathlib import Path
import sqlite3

ROOT=Path(__file__).resolve().parent
V3=ROOT/'results/v3'


def main():
    metrics=json.loads((V3/'evidence/metrics.json').read_text())['formal']
    db=sqlite3.connect(':memory:')
    db.executescript('''
    CREATE TABLE runs (id TEXT PRIMARY KEY, alias TEXT, category TEXT, status TEXT, passed INTEGER);
    CREATE TABLE acts (run_id TEXT, num INTEGER, response INTEGER, ran INTEGER, changed INTEGER, correct INTEGER, outcome TEXT, committed INTEGER);
    CREATE TABLE events (run_id TEXT, num INTEGER, op TEXT);
    ''')
    for path in sorted((V3/'formal').glob('*.jsonl')):
        rows=[json.loads(s) for s in path.read_text().splitlines()]
        s=rows[-1]
        assert s['type']=='summary','存在未结束运行'
        db.execute('INSERT INTO runs VALUES (?,?,?,?,?)',(s['run_id'],s['alias'],s['task'].split('-')[0],s['status'],s['passed']))
        for c in rows:
            if c['type']!='act':continue
            o=c['outcome']
            db.execute('INSERT INTO acts VALUES (?,?,?,?,?,?,?,?)',(s['run_id'],c['call'],c['response_index'],o['ran'],bool(o.get('changes')),c['candidate_files_correct'],o['outcome'],o.get('commit_called',False)))
            db.executemany('INSERT INTO events VALUES (?,?,?)',[(s['run_id'],c['call'],e['op']) for e in o.get('events',[])])
    result={}
    for alias in ['deepseek','minimax','all']:
        scope="r.status='complete'"+(" AND r.alias=?" if alias!='all' else '')
        args=(alias,) if alias!='all' else ()
        def query(sql):return db.execute(sql,args).fetchone()[0]
        def check(name,n,d):
            got={'n':n,'d':d}
            wanted={k:metrics[alias][name][k] for k in ('n','d')}
            assert got==wanted,f'独立重算不一致：{alias} {name} {got} {wanted}'
            result.setdefault(alias,{})[name]=got
        check('completion',query('SELECT count(*) FROM runs r WHERE '+scope+' AND passed=1'),query('SELECT count(*) FROM runs r WHERE '+scope))
        keep=scope+" AND r.category IN ('edit','batch','test')"
        base='FROM acts a JOIN runs r ON r.id=a.run_id WHERE '+keep
        correct=base+' AND a.ran=1 AND a.changed=1 AND a.correct=1'
        missed=correct+" AND a.outcome='uncommitted_reverted'"
        check('missed_correct_acts',query('SELECT count(*) '+missed),query('SELECT count(*) '+correct))
        check('missed_correct_runs',query('SELECT count(DISTINCT a.run_id) '+missed),query('SELECT count(*) FROM runs r WHERE '+keep))
        check('rescued_runs',query('SELECT count(DISTINCT a.run_id) '+missed+' AND r.passed=1'),query('SELECT count(DISTINCT a.run_id) '+missed))
        denominator=query('SELECT count(*) '+base+" AND a.outcome='uncommitted_reverted'")
        numerator=query('SELECT count(*) FROM acts a JOIN acts n ON n.run_id=a.run_id AND n.num=a.num+1 JOIN runs r ON r.id=a.run_id WHERE '+keep+" AND a.outcome='uncommitted_reverted' AND n.outcome='committed' AND n.response>a.response")
        wanted=metrics[alias]['keep_recovery']['next_rewrite_commit_all']
        assert (numerator,denominator)==(wanted['n'],wanted['d']),'下一次补救重算不符'
        result[alias]['next_act_recovery']=dict(n=numerator,d=denominator)
        numerator=query('SELECT count(*) FROM acts a JOIN acts n ON n.run_id=a.run_id AND n.num=a.num+1 JOIN runs r ON r.id=a.run_id WHERE '+keep+" AND a.correct=1 AND a.outcome='uncommitted_reverted' AND n.outcome='committed' AND n.response>a.response")
        denominator=query('SELECT count(*) '+missed)
        wanted=metrics[alias]['omission_recovery']['next_rewrite_commit_all']
        assert (numerator,denominator)==(wanted['n'],wanted['d']),'正确改动遗漏后的补救重算不符'
        result[alias]['correct_omission_recovery']=dict(n=numerator,d=denominator)
        read=scope+" AND r.category IN ('read','probe')"
        actual=query("SELECT count(*) FROM events e JOIN runs r ON r.id=e.run_id WHERE "+read+" AND e.op='commit'")
        assert actual==metrics[alias]['read_commit_calls'],'只读 commit 计数不符'
        result[alias]['read_commit_calls']=actual
    db.close()
    (V3/'evidence/independent-crosscheck.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps(result,ensure_ascii=False))


if __name__=='__main__':main()
