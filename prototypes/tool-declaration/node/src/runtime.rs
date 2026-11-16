//! `wasmtime` 49：只提供宿主接口的链接器、按次实例化与动态调用。

use std::collections::{BTreeMap, HashMap};

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde_json::{Value, json};
use wasmtime::component::types::{ComponentItem, Type};
use wasmtime::component::{
    Component, Func, Instance, Linker, Resource, ResourceAny, ResourceType, Val,
};
use wasmtime::{Config, Engine, Store, StoreContextMut};

use crate::decl::HOST_INTERFACE;

pub struct ReadDirRep;
pub struct WriteDirRep;
pub struct ExecEnvRep;
pub struct EgressRep;

/// 宿主资源实现记下的实际动作。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Observed {
    pub rep: u32,
    pub action: &'static str,
}

#[derive(Default)]
pub struct Host {
    pub dirs: HashMap<u32, BTreeMap<String, String>>,
    pub observed: Vec<Observed>,
    pub write_calls: u32,
}

pub const DOCS_REP: u32 = 1;
pub const WORK_REP: u32 = 2;
pub const NET_REP: u32 = 3;

pub fn engine() -> Result<Engine> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    Ok(Engine::new(&config)?)
}

fn read(host: &mut Host, rep: u32, path: &str) -> Result<String, String> {
    host.observed.push(Observed {
        rep,
        action: "Read",
    });
    host.dirs
        .get(&rep)
        .and_then(|d| d.get(path))
        .cloned()
        .ok_or_else(|| format!("{path}: 不存在"))
}

fn entries(host: &mut Host, rep: u32) -> Result<Vec<String>, String> {
    host.observed.push(Observed {
        rep,
        action: "Read",
    });
    Ok(host
        .dirs
        .get(&rep)
        .map(|d| d.keys().cloned().collect())
        .unwrap_or_default())
}

/// 节点的链接器只定义 `lancet:host/handles`，不提供 WASI。
pub fn linker(engine: &Engine) -> Result<Linker<Host>> {
    let mut linker = Linker::<Host>::new(engine);
    let mut i = linker.instance(HOST_INTERFACE)?;
    i.resource(
        "read-dir",
        ResourceType::host::<ReadDirRep>(),
        |_, _| Ok(()),
    )?;
    i.resource("write-dir", ResourceType::host::<WriteDirRep>(), |_, _| {
        Ok(())
    })?;
    i.resource(
        "exec-env",
        ResourceType::host::<ExecEnvRep>(),
        |_, _| Ok(()),
    )?;
    i.resource("egress", ResourceType::host::<EgressRep>(), |_, _| Ok(()))?;
    i.func_wrap(
        "[method]read-dir.read",
        |mut cx: StoreContextMut<'_, Host>,
         (this, path): (Resource<ReadDirRep>, String)|
         -> wasmtime::Result<(Result<String, String>,)> {
            Ok((read(cx.data_mut(), this.rep(), &path),))
        },
    )?;
    i.func_wrap(
        "[method]read-dir.entries",
        |mut cx: StoreContextMut<'_, Host>,
         (this, _path): (Resource<ReadDirRep>, String)|
         -> wasmtime::Result<(Result<Vec<String>, String>,)> {
            Ok((entries(cx.data_mut(), this.rep()),))
        },
    )?;
    i.func_wrap(
        "[method]write-dir.read",
        |mut cx: StoreContextMut<'_, Host>,
         (this, path): (Resource<WriteDirRep>, String)|
         -> wasmtime::Result<(Result<String, String>,)> {
            Ok((read(cx.data_mut(), this.rep(), &path),))
        },
    )?;
    i.func_wrap(
        "[method]write-dir.entries",
        |mut cx: StoreContextMut<'_, Host>,
         (this, _path): (Resource<WriteDirRep>, String)|
         -> wasmtime::Result<(Result<Vec<String>, String>,)> {
            Ok((entries(cx.data_mut(), this.rep()),))
        },
    )?;
    i.func_wrap(
        "[method]write-dir.write",
        |mut cx: StoreContextMut<'_, Host>,
         (this, path, content): (Resource<WriteDirRep>, String, String)|
         -> wasmtime::Result<(Result<(), String>,)> {
            let host = cx.data_mut();
            host.write_calls += 1;
            host.observed.push(Observed {
                rep: this.rep(),
                action: "Write",
            });
            host.dirs
                .entry(this.rep())
                .or_default()
                .insert(path, content);
            Ok((Ok(()),))
        },
    )?;
    i.func_wrap(
        "[method]exec-env.run",
        |mut cx: StoreContextMut<'_, Host>,
         (this, _argv): (Resource<ExecEnvRep>, Vec<String>)|
         -> wasmtime::Result<(Result<i32, String>,)> {
            cx.data_mut().observed.push(Observed {
                rep: this.rep(),
                action: "Exec",
            });
            Ok((Ok(0),))
        },
    )?;
    i.func_wrap(
        "[method]egress.fetch",
        |mut cx: StoreContextMut<'_, Host>,
         (this, url): (Resource<EgressRep>, String)|
         -> wasmtime::Result<(Result<String, String>,)> {
            cx.data_mut().observed.push(Observed {
                rep: this.rep(),
                action: "Emit",
            });
            Ok((Ok(format!("<html>{url}</html>")),))
        },
    )?;
    Ok(linker)
}

/// 一次工具调用的实例：每次调用新建 `Store` 与实例。
pub struct Session {
    pub store: Store<Host>,
    pub instance: Instance,
}

pub fn instantiate(
    engine: &Engine,
    linker: &Linker<Host>,
    component: &Component,
) -> Result<Session> {
    let mut host = Host::default();
    host.dirs.insert(
        DOCS_REP,
        BTreeMap::from([
            ("a.md".to_string(), "TODO: one\nok\n".to_string()),
            ("b.md".to_string(), "x\nTODO: two\n".to_string()),
        ]),
    );
    host.dirs.insert(
        WORK_REP,
        BTreeMap::from([("notes.md".to_string(), "first\n".to_string())]),
    );
    let mut store = Store::new(engine, host);
    let instance = linker.instantiate(&mut store, component)?;
    Ok(Session { store, instance })
}

impl Session {
    pub fn func(&mut self, iface: &str, name: &str) -> Result<Func> {
        let iface_index = self
            .instance
            .get_export_index(&mut self.store, None, iface)
            .ok_or_else(|| anyhow!("没有导出接口 {iface}"))?;
        let index = self
            .instance
            .get_export_index(&mut self.store, Some(&iface_index), name)
            .ok_or_else(|| anyhow!("没有导出函数 {name}"))?;
        self.instance
            .get_func(&mut self.store, &index)
            .ok_or_else(|| anyhow!("{name} 不是函数"))
    }

    pub fn read_dir(&mut self, rep: u32) -> Result<Val> {
        Ok(Val::Resource(ResourceAny::try_from_resource(
            Resource::<ReadDirRep>::new_own(rep),
            &mut self.store,
        )?))
    }

    pub fn write_dir(&mut self, rep: u32) -> Result<Val> {
        Ok(Val::Resource(ResourceAny::try_from_resource(
            Resource::<WriteDirRep>::new_own(rep),
            &mut self.store,
        )?))
    }

    pub fn egress(&mut self, rep: u32) -> Result<Val> {
        Ok(Val::Resource(ResourceAny::try_from_resource(
            Resource::<EgressRep>::new_own(rep),
            &mut self.store,
        )?))
    }

    pub fn call(&mut self, iface: &str, name: &str, args: &[Val]) -> Result<Val> {
        let func = self.func(iface, name)?;
        let mut results = vec![Val::Bool(false)];
        func.call(&mut self.store, args, &mut results)?;
        Ok(results.remove(0))
    }
}

pub fn val_json(val: &Val) -> Value {
    match val {
        Val::Bool(b) => json!(b),
        Val::U32(n) => json!(n),
        Val::S32(n) => json!(n),
        Val::U64(n) => json!(n),
        Val::S64(n) => json!(n),
        Val::String(s) => json!(s),
        Val::List(items) => Value::Array(items.iter().map(val_json).collect()),
        Val::Record(fields) => Value::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), val_json(v)))
                .collect(),
        ),
        Val::Option(o) => o.as_ref().map_or(Value::Null, |v| val_json(v)),
        Val::Result(Ok(v)) => json!({ "ok": v.as_ref().map_or(Value::Null, |v| val_json(v)) }),
        Val::Result(Err(v)) => json!({ "err": v.as_ref().map_or(Value::Null, |v| val_json(v)) }),
        other => json!(format!("{other:?}")),
    }
}

pub fn short_type(ty: &Type) -> String {
    match ty {
        Type::Bool => "bool".into(),
        Type::U32 => "u32".into(),
        Type::S32 => "s32".into(),
        Type::String => "string".into(),
        Type::List(l) => format!("list<{}>", short_type(&l.ty())),
        Type::Option(o) => format!("option<{}>", short_type(&o.ty())),
        Type::Result(r) => format!(
            "result<{}, {}>",
            r.ok().map_or("_".to_string(), |t| short_type(&t)),
            r.err().map_or("_".to_string(), |t| short_type(&t))
        ),
        Type::Record(_) => "record".into(),
        Type::Borrow(_) => "borrow<resource>".into(),
        Type::Own(_) => "own<resource>".into(),
        other => format!("{other:?}"),
    }
}

/// 从 `wasmtime` 的组件类型（预编译产物同样可用）读出导入名与导出函数签名。
pub fn component_signature(engine: &Engine, component: &Component) -> (Vec<String>, Vec<String>) {
    let ty = component.component_type();
    let imports = ty
        .imports(engine)
        .map(|(name, _)| name.to_string())
        .collect();
    let mut exports = Vec::new();
    for (name, item) in ty.exports(engine) {
        if let ComponentItem::ComponentInstance(instance) = item.ty {
            for (fname, fitem) in instance.exports(engine) {
                if let ComponentItem::ComponentFunc(f) = fitem.ty {
                    let params: Vec<String> = f
                        .params()
                        .map(|(n, t)| format!("{n}: {}", short_type(&t)))
                        .collect();
                    let results: Vec<String> = f.results().map(|t| short_type(&t)).collect();
                    exports.push(format!(
                        "{name}#{fname}({}) -> {}",
                        params.join(", "),
                        results.join(", ")
                    ));
                }
            }
        }
    }
    (imports, exports)
}
