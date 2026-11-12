"""同时保留源代码调用点与实际宿主事件，避免把未执行分支当作执行次数。"""
from collections import Counter
import io
import re
import tokenize

CLASSES = {
    'read': ('read', 'list', 'search', 'status', 'diff'),
    'write': ('write', 'edit', 'reset'),
    'execute': ('run', 'pipe'),
    'discard': ('discard',),
    'fail': ('fail',),
}
INTERFACES = {name for names in CLASSES.values() for name in names}


def source_calls(script):
    tokens=[]
    try:
        for tok in tokenize.generate_tokens(io.StringIO(script).readline):
            if tok.type in (tokenize.NAME, tokenize.OP):
                tokens.append(tok.string)
    except (tokenize.TokenError, IndentationError):
        pass
    counts=Counter()
    for i, token in enumerate(tokens):
        if i+1>=len(tokens) or tokens[i+1]!='(':
            continue
        if i>=2 and tokens[i-1]=='.' and tokens[i-2] in ('repo','env'):
            counts[tokens[i-2]+'.'+token]+=1
        elif token in ('discard','fail','print') and (i==0 or tokens[i-1] not in ('.','def')):
            counts[token]+=1
    return dict(counts)


def act_metrics(script, outcome):
    source=source_calls(script)
    executed=Counter(e['op'] for e in outcome.get('events',[]) if e['op'] in INTERFACES)
    # fail 是解释器原生函数；沿用 v1 的报错行定位，不替换其实现。
    line=outcome.get('line'); lines=script.splitlines()
    if not outcome.get('ran') and line and 0<line<=len(lines) and re.search(r'\bfail\s*\(',lines[line-1]):
        executed['fail']+=1
    categories={k:sum(executed[n] for n in names) for k,names in CLASSES.items()}
    source_categories={k:sum(source.get(('repo.' if n in CLASSES['read']+CLASSES['write'] else 'env.' if n in CLASSES['execute'] else '')+n,0) for n in names) for k,names in CLASSES.items()}
    return dict(interfaces=dict(executed),category_counts=categories,
                source_interfaces=source,source_category_counts=source_categories,
                read_only=categories['read']>0 and not any(categories[k] for k in ('write','execute','discard','fail')),
                single_interface=len(executed)==1,single_call=sum(executed.values())==1,
                print_calls=source.get('print',0))
