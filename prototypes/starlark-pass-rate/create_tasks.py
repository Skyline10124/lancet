"""从固定规格生成任务、夹具和判定入口；正式运行前执行。"""
import json
from pathlib import Path
ROOT = Path(__file__).resolve().parent


def task(ident, category, goal, files, *, facts=(), expected=None, command=None, failures=(), reason=None, replacement=None):
    dest = ROOT / 'tasks' / ident
    dest.mkdir(parents=True, exist_ok=True)
    (dest / 'task.md').write_text(goal.strip() + '\n')
    for path, content in files.items():
        p = dest / 'fixture' / path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content)
    spec = dict(id=ident, category=category, facts=list(facts), expected=expected if expected is not None else files, command=command, failures=list(failures), reason=reason, replacement=replacement)
    (dest / 'spec.json').write_text(json.dumps(spec, ensure_ascii=False, indent=2) + '\n')
    canned = []
    if command:
        log = f'\nrunning {1 + len(failures)} tests\ntest smoke::ok ... ok\n' + ''.join(f'test {name} ... FAILED\n' for name in failures)
        log += '\nfailures:\n' + ''.join(f"---- {name} stdout ----\nthread '{name}' panicked: {reason}\n" for name in failures)
        log += '\nfailures:\n' + ''.join(f'    {name}\n' for name in failures) + f'\ntest result: FAILED. 1 passed; {len(failures)} failed; 0 ignored\n'
        canned = [dict(argv=command, code=101, stdout=log, stderr='error: test failed\n')]
    (dest / 'canned.json').write_text(json.dumps(canned, indent=2) + '\n')
    (dest / 'check.py').write_text('from pathlib import Path\nimport sys\nsys.path.insert(0, str(Path(__file__).resolve().parents[2]))\nfrom check import cli\n\nif __name__ == "__main__":\n    cli(Path(__file__).resolve().parent)\n')


task('read-01','read','Find every TODO in src/. Report each file path, line number, and TODO text. Do not change files.',
     {'src/a.rs':'fn main() {}\n// TODO cache_limit\n','src/b.rs':'// ready\n// note\n// TODO retry_jitter\n','notes.txt':'TODO outside scope\n'}, facts=['src/a.rs','2','TODO cache_limit','src/b.rs','3','TODO retry_jitter'])
task('read-02','read','In logs/service.log, find ALERT and report its line plus the immediately preceding and following lines. Do not change files.',
     {'logs/service.log':'boot\nrequest id=R71\nALERT queue stalled\nworker id=W09\nhealthy\n'}, facts=['request id=R71','ALERT queue stalled','worker id=W09'])
task('read-03','read','Count the lines in each .txt file under data/, including blank lines. Report the count per file and the total. Do not change files.',
     {'data/a.txt':'alpha\n\nbeta\n','data/b.txt':'one\ntwo\nthree\nfour\n','data/ignore.md':'ignore\n'}, facts=['a.txt','3','b.txt','4','7'])
task('read-04','read','Find all FIXME comments in src/. Report their paths, line numbers, and owners. Do not change files.',
     {'src/a.py':'# FIXME owner=Mira: retry\nx = 1\n','src/sub/b.py':'x = 2\n# FIXME owner=Noah: cache\n','docs/note.md':'FIXME owner=Zed\n'}, facts=['src/a.py','1','Mira','src/sub/b.py','2','Noah'])

edits = [
 ('edit-01','config.json','{\n  "timeout_ms": 500,\n  "retries": 3\n}\n','500','800','Change timeout_ms in config.json from 500 to 800; preserve all other content.'),
 ('edit-02','src/options.rs','pub const STRICT: bool = false;\npub const TRACE: bool = false;\n','STRICT: bool = false','STRICT: bool = true','Enable STRICT in src/options.rs; keep TRACE and all other content unchanged.'),
 ('edit-03','README.md','# Client\nPrimary endpoint: /v1/query\nLegacy endpoint: /v1/query\n','Primary endpoint: /v1/query','Primary endpoint: /v2/query','Update only the primary endpoint in README.md to /v2/query; keep the legacy endpoint and formatting unchanged.'),
 ('edit-04','src/settings.py','PORT = 8080\nHEALTH_PORT = 8081\nLABEL = "gateway"\n','PORT = 8080','PORT = 9090','Change PORT in src/settings.py from 8080 to 9090; preserve HEALTH_PORT, LABEL and formatting.')]
for ident,path,content,old,new,goal in edits:
    task(ident,'edit',goal,{path:content},expected={path:content.replace(old,new)},replacement=[path,old,new])

for i,(old,new) in enumerate([('OldClient','NewClient'),('legacy_flag','modern_flag'),('slow_path','fast_path'),('V1_TAG','V2_TAG')],1):
    files = {'src/a.txt':f'{old}\ncall {old}\nkeep stable\n','src/nested/b.txt':f'use {old}\nreturn {old}\n{old}\n','docs/history.txt':f'Historical name: {old}\n','src/untouched.txt':'stable marker\n'}
    expected = {p:c.replace(old,new) if p.startswith('src/') else c for p,c in files.items()}
    task(f'batch-{i:02}','batch',f'Rename every occurrence of {old} to {new} in all files under src/, including subdirectories. Preserve every other character and leave docs/ unchanged.',files,expected=expected,replacement=['src/',old,new])

for i,(names,cmd,reason) in enumerate([
 (['parser::empty_input','parser::bad_utf8'],['cargo','test'],'invalid token at offset 3'),
 (['cache::expiry','cache::eviction'],['cargo','test','--offline'],'expected size 2, got 3'),
 (['net::retry_budget','net::deadline'],['cargo','test','--workspace'],'deadline exceeded after 50ms'),
 (['codec::roundtrip','codec::truncated_frame'],['cargo','test','--lib'],'checksum mismatch')],1):
    task(f'test-{i:02}','test',f'Run {" ".join(cmd)} and report every failing test name and its failure message. Do not change any files.',{'Cargo.toml':'[package]\nname = "sample"\nversion = "0.1.0"\n','src/lib.rs':'pub fn value() -> i32 { 1 }\n'},command=cmd,failures=names,reason=reason,facts=[*names,reason])

for i,(path,content,old,new,names,reason) in enumerate([
 ('src/limits.rs','pub const LIMIT: u32 = 4;\n','4','8',['limits::overflow'],'expected rejection at 5'),
 ('src/cache.rs','pub const CAPACITY: usize = 16;\n','16','32',['cache::memory_budget'],'allocation budget exceeded'),
 ('src/retry.rs','pub const RETRIES: u32 = 2;\n','2','3',['retry::deadline'],'request deadline exceeded'),
 ('src/codec.rs','pub const VERSION: u8 = 1;\n','1','2',['codec::compatibility'],'legacy frame rejected')],1):
    task(f'rollback-{i:02}','rollback',f'Change the value {old} to {new} in {path}, then run cargo test. If any test fails, do not keep your change; report which tests failed and why. If all tests pass, keep the change and report the test summary.',{path:content,'Cargo.toml':'[package]\nname = "sample"\nversion = "0.1.0"\n'},command=['cargo','test'],failures=names,reason=reason,facts=[*names,reason],replacement=[path,f'= {old};',f'= {new};'])

task('probe-01','probe','Inspect config/optional.json and config/default.json. If optional.json is absent, report "optional missing" and the default region. Do not create or change files.', {'config/default.json':'{"region":"eu-north-7"}\n'},facts=['optional missing','eu-north-7'])
task('probe-02','probe','Read services.json and report a JSON object with enabled service names sorted alphabetically, and the sum of their ports. Do not change files.',{'services.json':'{"services":[{"name":"zeta","enabled":true,"port":8100},{"name":"alpha","enabled":true,"port":8200},{"name":"beta","enabled":false,"port":9999}]}\n'},facts=['alpha','zeta','16300'])
large = ''.join(f'INFO heartbeat {i:05} status=healthy padding=abcdefghij\n' for i in range(2400))
large += 'ERROR incident=E701 shard=west reason=disk_full\n'
large += ''.join(f'INFO heartbeat {i:05} status=healthy padding=abcdefghij\n' for i in range(2400,3600))
large += 'ERROR incident=E702 shard=east reason=timeout\n'
task('probe-03','probe','Find the ERROR entries in logs/large.log. Report each incident id, shard, and reason, and the total number of ERROR entries. Do not change files.',{'logs/large.log':large},facts=['E701','west','disk_full','E702','east','timeout','2'])
task('probe-04','probe','Read deploy.json and check whether overrides.json exists. Report a JSON object containing the sorted names of prod targets, their total replicas, and whether overrides.json is present. Do not change files.',{'deploy.json':'{"targets":[{"name":"west","env":"prod","replicas":3},{"name":"dev","env":"test","replicas":9},{"name":"east","env":"prod","replicas":2}]}\n','overrides.json':'{}\n'},facts=['east','west','5','true'])

if __name__ == '__main__':
    print('生成 24 个固定任务')
