#!/usr/bin/env python3
"""私有 Git 的 plumbing 辅助程序。事务应用与恢复统一交给 Rust；本程序只准备数据和持锁。"""
import hashlib,json,os,shutil,subprocess,sys,time
from pathlib import Path

def git(root,*args,input=None,index=None):
    env=dict(os.environ,GIT_CONFIG_NOSYSTEM='1',GIT_CONFIG_GLOBAL='/dev/null',GIT_OPTIONAL_LOCKS='0')
    if root==shadow:env['GIT_ALTERNATE_OBJECT_DIRECTORIES']=str(real/'.git/objects')
    if index:env['GIT_INDEX_FILE']=str(index)
    p=subprocess.run(['git','-C',str(root),'-c','core.hooksPath=/dev/null','-c','core.fsmonitor=false','-c','user.name=bench','-c','user.email=bench@example.invalid',*args],input=input,capture_output=True,env=env)
    if p.returncode:raise RuntimeError(p.stderr.decode(errors='replace'))
    return p.stdout

def refs(root):
    return dict(x.split(' ',1)[::-1] for x in git(root,'for-each-ref','--format=%(objectname) %(refname)').decode().splitlines())
def idx(root):
    out={}
    for x in git(root,'ls-files','--stage','-z').split(b'\0'):
        if x:
            spec,p=x.split(b'\t',1);out[p.decode()]=spec.decode()
    return out

def fingerprint(p):
    return hashlib.sha256(p.read_bytes()).hexdigest() if p.is_file() else None

def protected(root):
    return {str(p.relative_to(root)):fingerprint(p) for p in root.rglob('*') if p.is_file() and p.relative_to(root).parts[0] not in ['objects','host-objects-v6','refs','logs'] and p.name not in ['HEAD','index','packed-refs','COMMIT_EDITMSG','ORIG_HEAD','FETCH_HEAD'] and not p.name.endswith('.lock')}

def sync():
    if not (real/'.git/HEAD').is_file():return {'enabled':False}
    start=time.monotonic();g=shadow/'.git';g.mkdir(exist_ok=True)
    for p in list(g.iterdir()):
        if p.name not in ['objects','host-objects-v6']:
            if p.is_dir():shutil.rmtree(p)
            else:p.unlink()
    for p in (real/'.git').iterdir():
        if p.name in ['objects','host-objects-v6'] or p.name.endswith('.lock'):continue
        dst=g/p.name
        if p.is_dir():shutil.copytree(p,dst,copy_function=clone,ignore=shutil.ignore_patterns('*.lock'))
        else:clone(p,dst)
    (g/'objects/info').mkdir(parents=True,exist_ok=True);(g/'host-objects-v6').mkdir(exist_ok=True)
    (g/'objects/info/alternates').write_text(str(real/'.git/host-objects-v6')+'\n')
    # 私有暂存区从 HEAD 开始；宿主既有 stage 不自动成为 agent commit 的一部分。
    git(shadow,'read-tree','HEAD')
    base={'head':(real/'.git/HEAD').read_text(),'oid':git(real,'rev-parse','HEAD').decode().strip(),'refs':refs(real),'index':idx(real),'protected':protected(g)}
    (state/'git-base-v6.json').write_text(json.dumps(base));return {'enabled':True,'git_sync_s':time.monotonic()-start,'index_bytes':(g/'index').stat().st_size}

def prepare():
    if not (real/'.git/HEAD').is_file():return {'paths':[],'locks':[],'conflicts':[]}
    start=time.monotonic();base=json.loads((state/'git-base-v6.json').read_text());g=shadow/'.git';out=state/'git-merge-v6'
    shutil.rmtree(out,ignore_errors=True);out.mkdir()
    newrefs=refs(shadow);newhead=(g/'HEAD').read_text();newid=git(shadow,'rev-parse','HEAD').decode().strip()
    changedrefs=[p for p in base['refs'].keys()|newrefs.keys() if base['refs'].get(p)!=newrefs.get(p)]
    protected_now=protected(g);blocked=[p for p in base['protected'].keys()|protected_now.keys() if base['protected'].get(p)!=protected_now.get(p)]
    staged=git(shadow,'diff','--cached','--name-only','-z').decode().split('\0')[:-1]
    paths=[];locks=[];conflicts=[]
    if newid!=base['oid'] or newhead!=base['head'] or changedrefs:
        names=['HEAD','index','packed-refs',*changedrefs]
        for p in sorted(set(names)):
            l=real/'.git'/(p+'.lock');l.parent.mkdir(parents=True,exist_ok=True)
            try:fd=os.open(l,os.O_CREAT|os.O_EXCL|os.O_WRONLY,0o600);os.close(fd);locks.append('.git/'+p+'.lock')
            except FileExistsError:conflicts.append('.git/'+p)
        token=str(state)+'\n'
        for name in locks:
            with open(real/name,'w') as f:f.write(token)
        manifest=state/'git-locks-v6.json'
        with manifest.open('w') as f:json.dump(dict(token=token,paths=locks),f);f.flush();os.fsync(f.fileno())
        nowrefs=refs(real)
        if (real/'.git/HEAD').read_text()!=base['head'] or git(real,'rev-parse','HEAD').decode().strip()!=base['oid']:conflicts.append('.git/HEAD')
        conflicts+=['.git/'+p for p in changedrefs if nowrefs.get(p)!=base['refs'].get(p)]
        changed=git(shadow,'diff-tree','--no-commit-id','--name-only','-r','-z',base['oid'],newid).decode().split('\0')[:-1] if newid!=base['oid'] else []
        nowidx=idx(real)
        conflicts+=['.git/index:'+p for p in changed if nowidx.get(p)!=base['index'].get(p)]
        if not conflicts:
            # 验证私有对象数据库；不把 config/hook 交给宿主 git。
            subprocess.run([str(Path(__file__).parent/'.scratch/rust-target-v6/release/shadow-v6'),'verify-git-objects',str(shadow)],check=True,capture_output=True)
            git(shadow,'fsck','--no-reflogs','--connectivity-only')
            objects=[]
            for p in (g/'objects').rglob('*'):
                rel=p.relative_to(g/'objects')
                if not p.is_file() or rel.parts[0]=='info':continue
                if len(rel.parts)==2 and len(rel.parts[0])==2:
                    pass  # gix 已逐个解析并核对对象摘要。
                else:raise RuntimeError('原型只合并 loose objects；pack 对象未实现')
                dst=real/'.git/objects'/rel;dst.parent.mkdir(exist_ok=True)
                if not dst.exists():clone(p,dst)
                objects.append(str(rel))
            if newhead!=base['head']:(out/'HEAD').write_text(newhead);paths.append('.git/HEAD')
            for p in changedrefs:
                dst=out/p;dst.parent.mkdir(parents=True,exist_ok=True)
                if p in newrefs:dst.write_text(newrefs[p]+'\n')
                paths.append('.git/'+p)
            # 删除 packed ref 时也去掉该名字；其他 packed 引用原样保留。
            if any(p not in newrefs for p in changedrefs) and (real/'.git/packed-refs').exists():
                lines=(real/'.git/packed-refs').read_text().splitlines(True);kept=[];drop=False
                for line in lines:
                    if line.startswith('^'):
                        if not drop:kept.append(line)
                    else:
                        drop=any(line.rstrip().endswith(' '+p) for p in changedrefs if p not in newrefs)
                        if not drop:kept.append(line)
                (out/'packed-refs').write_text(''.join(kept));paths.append('.git/packed-refs')
            if changed:
                clone(real/'.git/index',out/'index');tree={}
                for rec in git(shadow,'ls-tree','-r','-z',newid).split(b'\0'):
                    if rec:
                        spec,p=rec.split(b'\t',1);mode,kind,oid=spec.split();tree[p.decode()]=(mode,oid)
                data=b''
                for p in changed:
                    mode,oid=tree.get(p,(b'0',b'0'*40));data+=mode+b' '+oid+b'\t'+p.encode()+b'\0'
                git(real,'update-index','-z','--index-info',input=data,index=out/'index');paths.append('.git/index')
    return {'paths':paths,'locks':locks,'conflicts':sorted(set(conflicts)),'blocked':sorted(blocked),'staged_not_merged':staged,'git_prepare_s':time.monotonic()-start}

def clone(src,dst):
    import fcntl
    with open(src,'rb') as a,open(dst,'wb') as b:fcntl.ioctl(b,0x40049409,a.fileno())
    shutil.copystat(src,dst);return str(dst)

if __name__=='__main__':
    op=sys.argv[1];real,state=map(Path,sys.argv[2:4]);shadow=state/'shadow'
    print(json.dumps(globals()[op]()))
