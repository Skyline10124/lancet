"""OpenRouter 费用账本：每次请求前查 usage，并为并发中请求保留上界。"""
import json
from pathlib import Path
import threading
import time
import urllib.request

ROOT = Path(__file__).resolve().parent
EVIDENCE = ROOT/'results/v3/evidence'
MAX_TOKENS = 8192


class BudgetStop(RuntimeError):
    pass


def read_key(path):
    key = Path(path).expanduser().read_text().strip().rstrip('~').strip()
    if not key:
        raise RuntimeError('密钥文件为空')
    return key


def usage(key):
    request = urllib.request.Request('https://openrouter.ai/api/v1/key', headers={'Authorization': 'Bearer '+key})
    with urllib.request.urlopen(request, timeout=30) as response:
        data = json.load(response)['data']
    return dict(time=time.time(), usage=data['usage'], usage_daily=data.get('usage_daily'))


class Budget:
    def __init__(self):
        self.lock = threading.Lock()
        self.reserved = 0.0
        self.local_cost = 0.0
        self.stopped = False
        self.baseline = json.loads((EVIDENCE/'usage-start.json').read_text())['usage']
        self.key = read_key('~/.config/llm-keys/openrouter.key')
        endpoints = json.loads((EVIDENCE/'deepseek-endpoints.json').read_text())['data']['endpoints']
        rates = [e['pricing'] for e in endpoints]
        rates += [r for e in endpoints for r in e['pricing'].get('overrides', [])]
        # UTF-8 字节数大于通常的 token 数；另加模板余量并将单价翻倍。
        self.prompt_price = max(float(r['prompt']) for r in rates)*2
        self.completion_price = max(float(r['completion']) for r in rates)*2
        for folder in ('pilot', 'formal'):
            for path in (ROOT/'results/v3'/folder).glob('*.jsonl'):
                for line in path.read_text().splitlines():
                    row = json.loads(line)
                    if row['type'] == 'api_response':
                        self.local_cost += (row['response'].get('usage') or {}).get('cost') or 0

    def admit(self, body):
        reserve = (len(json.dumps(body, ensure_ascii=False).encode())+2048)*self.prompt_price + MAX_TOKENS*self.completion_price
        with self.lock:
            if self.stopped:
                raise BudgetStop('OpenRouter 费用保护已停止实验')
            now = usage(self.key)
            spent = max(now['usage']-self.baseline, self.local_cost)
            allowed = spent+self.reserved+reserve <= 5
            with (EVIDENCE/'budget.jsonl').open('a') as log:
                log.write(json.dumps(dict(**now,spent=spent,reserved=self.reserved,next_reservation=reserve,allowed=allowed))+'\n')
            if not allowed:
                self.stopped = True
                raise BudgetStop('OpenRouter 费用及在途预留将超过 5 美元，停止发起新请求')
            self.reserved += reserve
        return reserve

    def finish(self, reserve, data):
        with self.lock:
            self.reserved -= reserve
            self.local_cost += (data.get('usage') or {}).get('cost') or 0
