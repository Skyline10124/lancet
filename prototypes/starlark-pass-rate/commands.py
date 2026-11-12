"""判定器与宿主共用显式归一化表；未知参数不降级匹配。"""
import json
from pathlib import Path

RULES = json.loads(Path(__file__).with_name('command-normalization.json').read_text())


def normalize(argv):
    if argv[:2] != RULES['prefix']:
        return argv
    scope = set()
    section = 'cargo'
    filters = 0
    i = 2
    while i < len(argv):
        arg = argv[i]
        i += 1
        if section == 'cargo' and arg == '--':
            section, filters = 'test', 0
        elif arg in RULES[section + '_ignored_flags']:
            continue
        elif section == 'cargo' and arg in RULES['cargo_scope_flags']:
            scope.add(arg)
        elif section == 'test' and arg.split('=')[0] in RULES['test_positive_integer_options']:
            if '=' in arg:
                value = arg.split('=', 1)[1]
            elif i < len(argv):
                value = argv[i]
                i += 1
            else:
                return argv
            if not value.isascii() or not value.isdigit() or int(value) <= 0:
                return argv
        elif arg and not arg.startswith('-') and filters < RULES[section + '_filter_limit']:
            filters += 1
        else:
            return argv
    return RULES['prefix'] + sorted(scope)
