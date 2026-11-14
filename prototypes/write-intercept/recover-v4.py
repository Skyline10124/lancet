#!/usr/bin/env python3
"""节点重启入口：恢复未完成事务，再从真实目录重建影子一致性。"""
import json
from pathlib import Path
import runpy
import sys
import time
v=runpy.run_path(str(Path(__file__).with_name('shadow-v4.py')))


def restart(real,state):
    start=time.monotonic();o=v['Shadow'](real,state);results=[]
    try:
        for txn in sorted((o.state/'transactions').glob('*')):
            if (txn/'transaction.json').exists():
                results.append(dict(transaction=str(txn),result=v['recover'](txn)))
            else:
                # preparing 日志尚未原子发布，没有任何真实目录应用许可。
                results.append(dict(transaction=str(txn),result={'outcome':'Unprepared'}))
        real_view=v['scan'](o.real);shadow_view=v['scan'](o.shadow)
        # 重启路径允许整树复制；不能相信被杀进程的持久同步索引。
        paths=real_view.keys()|shadow_view.keys()
        v['reconcile'](o.shadow,real_view,paths,lambda p:o.real/p)
        o.index={'real':real_view,'shadow':v['scan'](o.shadow)}
        v['save'](o.state/'index.json',o.index)
        (o.state/'begin.json').unlink(missing_ok=True)
        return dict(transactions=results,entries=len(real_view),restart_s=time.monotonic()-start)
    finally:o.close()


if __name__=='__main__':print(json.dumps(restart(sys.argv[1],sys.argv[2]),ensure_ascii=False))
