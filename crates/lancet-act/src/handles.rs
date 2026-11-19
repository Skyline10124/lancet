// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 句柄类型、记录类型与绑定方法的构造、静态类型建模与子调用分发。

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use allocative::Allocative;
use anyhow::anyhow;
use lancet_tool_decl::{
    ActContext, HandleKind, ParamDecl, RecordDecl, ReturnType, ToolDecl, WitType,
    first_optional_param, wit_to_ident, wit_to_type_name,
};
use starlark::any::ProvidesStaticType;
use starlark::environment::GlobalsBuilder;
use starlark::eval::{Arguments, Evaluator, ParametersSpec, ParametersSpecParam};
use starlark::starlark_simple_value;
use starlark::typing::{
    ParamIsRequired, ParamSpec, Ty, TyStarlarkValue, TyUser, TyUserFields, TyUserParams,
};
use starlark::values::float::UnpackFloat;
use starlark::values::list::ListRef;
use starlark::values::structs::AllocStruct;
use starlark::values::type_repr::StarlarkTypeRepr;
use starlark::values::typing::TypeInstanceId;
use starlark::values::{
    FrozenValue, Heap, NoSerialize, StarlarkPagablePanic, StarlarkValue, UnpackValue, Value,
    ValueLike, starlark_value,
};

use crate::lancet::act::dispatch;

/// 根据本次 `act` 上下文构造名义类型并在全局环境注入授权句柄及其绑定方法。
pub fn register_context(builder: &mut GlobalsBuilder, context: &ActContext) {
    if context.handles.is_empty() && context.tools.is_empty() {
        return;
    }

    let handle_ids: BTreeMap<HandleKind, TypeInstanceId> = HandleKind::ALL
        .into_iter()
        .map(|kind| (kind, TypeInstanceId::r#gen()))
        .collect();

    let mut handle_param_tys = BTreeMap::new();
    for kind in HandleKind::ALL {
        let user = TyUser::new(
            kind.stub_name().to_owned(),
            TyStarlarkValue::new::<HandleValue>(),
            handle_ids[&kind],
            TyUserParams::default(),
        )
        .expect("构造句柄参数名义类型失败");
        handle_param_tys.insert(kind, Ty::custom(user));
    }

    let (records_map, record_tys) = build_record_types(&context.tools, &handle_param_tys);
    let records = Arc::new(records_map);

    let mut per_kind_methods: BTreeMap<HandleKind, BTreeMap<String, Arc<MethodMeta>>> =
        HandleKind::ALL
            .into_iter()
            .map(|k| (k, BTreeMap::new()))
            .collect();

    for tool in &context.tools {
        let (recv_idx, recv_kind) = tool
            .receiver()
            .expect("API 工具声明必须含有一个 borrow 句柄接收者");
        let meta = Arc::new(build_method_meta(
            tool,
            recv_idx,
            Arc::clone(&records),
            &handle_param_tys,
            &record_tys,
        ));
        let method_name = meta.method_name.clone();
        for kind in HandleKind::ALL {
            if recv_kind.accepts(kind) {
                per_kind_methods
                    .get_mut(&kind)
                    .expect("全部句柄种类已初始化")
                    .insert(method_name.clone(), Arc::clone(&meta));
            }
        }
    }

    let mut handle_global_tys = BTreeMap::new();
    for kind in HandleKind::ALL {
        let known = per_kind_methods[&kind]
            .iter()
            .map(|(name, meta)| (name.clone(), meta.method_ty.clone()))
            .collect();
        let user = TyUser::new(
            kind.stub_name().to_owned(),
            TyStarlarkValue::new::<HandleValue>(),
            handle_ids[&kind],
            TyUserParams {
                fields: TyUserFields {
                    known,
                    unknown: false,
                },
                ..TyUserParams::default()
            },
        )
        .expect("构造句柄全局名义类型失败");
        handle_global_tys.insert(kind, Ty::custom(user));
    }

    for grant in &context.handles {
        let handle = HandleValue {
            name: grant.name.clone(),
            kind: grant.kind,
            slot: grant.slot,
            handle_ty: handle_global_tys[&grant.kind].clone(),
            methods: Arc::new(per_kind_methods[&grant.kind].clone()),
        };
        let frozen = builder.frozen_heap().alloc(handle);
        builder.set(&grant.name, frozen);
    }
}

fn build_record_types(
    tools: &[ToolDecl],
    handle_param_tys: &BTreeMap<HandleKind, Ty>,
) -> (BTreeMap<String, RecordDecl>, BTreeMap<String, Ty>) {
    let mut decls = BTreeMap::new();
    let mut tys = BTreeMap::new();

    for tool in tools {
        for record in &tool.records {
            if decls.contains_key(&record.name) {
                continue;
            }
            let known = record
                .fields
                .iter()
                .map(|field| {
                    (
                        wit_to_ident(&field.name),
                        wit_type_to_ty(&field.ty, false, handle_param_tys, &tys),
                    )
                })
                .collect();
            let user = TyUser::new(
                wit_to_type_name(&record.name),
                TyStarlarkValue::new::<RecordStub>(),
                TypeInstanceId::r#gen(),
                TyUserParams {
                    fields: TyUserFields {
                        known,
                        unknown: false,
                    },
                    ..TyUserParams::default()
                },
            )
            .expect("构造记录名义类型失败");
            decls.insert(record.name.clone(), record.clone());
            tys.insert(record.name.clone(), Ty::custom(user));
        }
    }

    (decls, tys)
}

fn build_method_meta(
    tool: &ToolDecl,
    recv_idx: usize,
    records: Arc<BTreeMap<String, RecordDecl>>,
    handle_param_tys: &BTreeMap<HandleKind, Ty>,
    record_tys: &BTreeMap<String, Ty>,
) -> MethodMeta {
    let method_name = tool.method_name();
    let bare_params: Vec<&ParamDecl> = tool.method_params().into_iter().map(|(_, p)| p).collect();
    let opt_start = first_optional_param(&bare_params);

    let mut static_params = Vec::with_capacity(bare_params.len());
    let mut runtime_params = Vec::with_capacity(bare_params.len());
    for (pos, param) in bare_params.iter().enumerate() {
        let ident = wit_to_ident(&param.name);
        let ty = wit_type_to_ty(&param.ty, true, handle_param_tys, record_tys);
        let is_opt = pos >= opt_start;
        static_params.push((
            ident.as_str().into(),
            if is_opt {
                ParamIsRequired::No
            } else {
                ParamIsRequired::Yes
            },
            ty,
        ));
        runtime_params.push((
            ident,
            if is_opt {
                ParametersSpecParam::Defaulted(FrozenValue::new_none())
            } else {
                ParametersSpecParam::Required
            },
        ));
    }

    let param_spec = ParamSpec::new_parts([], static_params, None, [], None)
        .expect("构造绑定方法静态参数签名失败");
    let ret_ty = tool.result.ok_type().map_or_else(Ty::none, |t| {
        wit_type_to_ty(t, false, handle_param_tys, record_tys)
    });
    let method_ty = Ty::function(param_spec, ret_ty);

    let params_spec = ParametersSpec::new_parts(
        &tool.script_name,
        [],
        runtime_params.iter().map(|(n, p)| (n.as_str(), *p)),
        false,
        [],
        false,
    );

    MethodMeta {
        tool_id: tool.wit_export(),
        method_name,
        recv_idx,
        tool: tool.clone(),
        records,
        params_spec,
        method_ty,
    }
}

fn wit_type_to_ty(
    ty: &WitType,
    is_param: bool,
    handle_param_tys: &BTreeMap<HandleKind, Ty>,
    record_tys: &BTreeMap<String, Ty>,
) -> Ty {
    match ty {
        WitType::Bool => Ty::bool(),
        WitType::String => Ty::string(),
        WitType::Int => Ty::int(),
        WitType::Float => {
            if is_param {
                <UnpackFloat as StarlarkTypeRepr>::starlark_type_repr()
            } else {
                Ty::float()
            }
        }
        WitType::Option(inner) => Ty::union2(
            wit_type_to_ty(inner, is_param, handle_param_tys, record_tys),
            Ty::none(),
        ),
        WitType::List(inner) => Ty::list(wit_type_to_ty(
            inner,
            is_param,
            handle_param_tys,
            record_tys,
        )),
        WitType::Record(name) => record_tys[name].clone(),
        WitType::Handle { kind, .. } if is_param => HandleKind::ALL
            .into_iter()
            .filter(|actual| kind.accepts(*actual))
            .map(|actual| handle_param_tys[&actual].clone())
            .reduce(Ty::union2)
            .expect("句柄种类至少接受自身"),
        WitType::Handle { kind, .. } => handle_param_tys[kind].clone(),
    }
}

/// 静态类型检查占位类型：让各记录名义类型拥有封闭已知字段并在属性访问失败时拦截。
#[derive(Debug, ProvidesStaticType, NoSerialize, StarlarkPagablePanic, Allocative)]
enum RecordStub {}

impl fmt::Display for RecordStub {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {}
    }
}

#[starlark_value(type = "lancet_record")]
impl<'v> StarlarkValue<'v> for RecordStub {}

#[derive(Debug, Allocative)]
struct MethodMeta {
    tool_id: String,
    method_name: String,
    recv_idx: usize,
    #[allocative(skip)]
    tool: ToolDecl,
    #[allocative(skip)]
    records: Arc<BTreeMap<String, RecordDecl>>,
    params_spec: ParametersSpec<FrozenValue>,
    method_ty: Ty,
}

#[derive(Debug, Clone, ProvidesStaticType, NoSerialize, StarlarkPagablePanic, Allocative)]
struct HandleValue {
    name: String,
    #[allocative(skip)]
    kind: HandleKind,
    slot: u32,
    handle_ty: Ty,
    #[allocative(skip)]
    methods: Arc<BTreeMap<String, Arc<MethodMeta>>>,
}

impl fmt::Display for HandleValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} {}>", self.kind.stub_name(), self.name)
    }
}

starlark_simple_value!(HandleValue);

#[starlark_value(type = "lancet_handle")]
impl<'v> StarlarkValue<'v> for HandleValue {
    fn typechecker_ty(&self) -> Option<Ty> {
        Some(self.handle_ty.clone())
    }

    fn get_attr(&self, attribute: &str, heap: Heap<'v>) -> Option<Value<'v>> {
        let meta = self.methods.get(attribute)?;
        Some(heap.alloc(BoundMethod {
            handle_slot: self.slot,
            meta: Arc::clone(meta),
        }))
    }

    fn has_attr(&self, attribute: &str, _heap: Heap<'v>) -> bool {
        self.methods.contains_key(attribute)
    }

    fn dir_attr(&self) -> Vec<String> {
        self.methods.keys().cloned().collect()
    }
}

#[derive(Debug, Clone, ProvidesStaticType, NoSerialize, StarlarkPagablePanic, Allocative)]
struct BoundMethod {
    handle_slot: u32,
    meta: Arc<MethodMeta>,
}

impl fmt::Display for BoundMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<bound method {}>", self.meta.tool.script_name)
    }
}

starlark_simple_value!(BoundMethod);

#[starlark_value(type = "lancet_method")]
impl<'v> StarlarkValue<'v> for BoundMethod {
    fn typechecker_ty(&self) -> Option<Ty> {
        Some(self.meta.method_ty.clone())
    }

    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let bare_count = self.meta.params_spec.len();
        let mut slots = vec![None; bare_count];
        self.meta
            .params_spec
            .collect(args, &mut slots, eval.heap())?;

        let mut dispatch_args = Vec::with_capacity(self.meta.tool.params.len());
        let mut slot_cursor = 0usize;
        for (idx, param) in self.meta.tool.params.iter().enumerate() {
            if idx == self.meta.recv_idx {
                dispatch_args.push(dispatch::Argument::HandleRef(self.handle_slot));
                continue;
            }
            let val = slots[slot_cursor].expect("ParametersSpec 已填充形参槽位");
            slot_cursor += 1;
            let encoded = encode_dispatch_arg(val, &param.ty, &param.name)?;
            dispatch_args.push(encoded);
        }

        let ok_json = dispatch::call(&self.meta.tool_id, &dispatch_args)
            .map_err(|msg| starlark::Error::new_other(anyhow!(msg)))?;

        decode_return_value(
            &self.meta.tool.result,
            &ok_json,
            &self.meta.records,
            eval.heap(),
        )
    }
}

fn encode_dispatch_arg<'v>(
    val: Value<'v>,
    ty: &WitType,
    param_name: &str,
) -> starlark::Result<dispatch::Argument> {
    if let WitType::Handle { kind, .. } = ty {
        let Some(handle) = val.downcast_ref::<HandleValue>() else {
            return Err(type_mismatch(param_name, kind.stub_name(), val));
        };
        if !kind.accepts(handle.kind) {
            return Err(type_mismatch(param_name, kind.stub_name(), val));
        }
        return Ok(dispatch::Argument::HandleRef(handle.slot));
    }

    let json_val = encode_json_value(val, ty, param_name)?;
    let encoded = serde_json::to_string(&json_val)
        .map_err(|e| starlark::Error::new_other(anyhow!("序列化参数 `{param_name}` 失败：{e}")))?;
    Ok(dispatch::Argument::ValueJson(encoded))
}

fn encode_json_value<'v>(
    val: Value<'v>,
    ty: &WitType,
    path: &str,
) -> starlark::Result<serde_json::Value> {
    match ty {
        WitType::Bool => {
            let b = val
                .unpack_bool()
                .ok_or_else(|| type_mismatch(path, "bool", val))?;
            Ok(serde_json::Value::Bool(b))
        }
        WitType::String => {
            let s = val
                .unpack_str()
                .ok_or_else(|| type_mismatch(path, "str", val))?;
            Ok(serde_json::Value::String(s.to_owned()))
        }
        WitType::Int => {
            if val.get_type() != "int" {
                return Err(type_mismatch(path, "int", val));
            }
            let json_text = val.to_json()?;
            let num: serde_json::Number = serde_json::from_str(&json_text).map_err(|e| {
                starlark::Error::new_other(anyhow!("参数 `{path}` 整数转换 JSON 失败：{e}"))
            })?;
            Ok(serde_json::Value::Number(num))
        }
        WitType::Float => {
            if val.get_type() != "int" && val.get_type() != "float" {
                return Err(type_mismatch(path, "float", val));
            }
            let Some(UnpackFloat(f)) = UnpackFloat::unpack_value(val)? else {
                return Err(type_mismatch(path, "float", val));
            };
            let num = serde_json::Number::from_f64(f).ok_or_else(|| {
                starlark::Error::new_other(anyhow!("参数 `{path}` 浮点数不可为 NaN 或无穷大"))
            })?;
            Ok(serde_json::Value::Number(num))
        }
        WitType::Option(inner) => {
            if val.is_none() {
                Ok(serde_json::Value::Null)
            } else {
                encode_json_value(val, inner, path)
            }
        }
        WitType::List(inner) => {
            let list = ListRef::from_value(val).ok_or_else(|| type_mismatch(path, "list", val))?;
            let mut items = Vec::with_capacity(list.len());
            for (idx, item) in list.iter().enumerate() {
                items.push(encode_json_value(item, inner, &format!("{path}[{idx}]"))?);
            }
            Ok(serde_json::Value::Array(items))
        }
        WitType::Record(_) => unreachable!("声明加载已拒绝记录参数"),
        WitType::Handle { .. } => unreachable!("声明加载已拒绝嵌套句柄参数"),
    }
}

fn decode_return_value<'v>(
    ret: &ReturnType,
    ok_json: &str,
    records: &BTreeMap<String, RecordDecl>,
    heap: Heap<'v>,
) -> starlark::Result<Value<'v>> {
    let Some(ty) = ret.ok_type() else {
        return Ok(Value::new_none());
    };
    let parsed: serde_json::Value = serde_json::from_str(ok_json)
        .map_err(|e| starlark::Error::new_other(anyhow!("解析子工具返回 JSON 失败：{e}")))?;
    decode_json_value(&parsed, ty, "return", records, heap)
}

fn decode_json_value<'v>(
    val: &serde_json::Value,
    ty: &WitType,
    path: &str,
    records: &BTreeMap<String, RecordDecl>,
    heap: Heap<'v>,
) -> starlark::Result<Value<'v>> {
    match ty {
        WitType::Bool => match val.as_bool() {
            Some(b) => Ok(Value::new_bool(b)),
            None => Err(json_mismatch(path, "bool", val)),
        },
        WitType::String => match val.as_str() {
            Some(s) => Ok(heap.alloc(s)),
            None => Err(json_mismatch(path, "string", val)),
        },
        WitType::Int => match val {
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(heap.alloc(i))
                } else if let Some(u) = n.as_u64() {
                    Ok(heap.alloc(u))
                } else {
                    Err(json_mismatch(path, "int", val))
                }
            }
            _ => Err(json_mismatch(path, "int", val)),
        },
        WitType::Float => match val.as_f64() {
            Some(f) => Ok(heap.alloc(f)),
            None => Err(json_mismatch(path, "float", val)),
        },
        WitType::Option(inner) => {
            if val.is_null() {
                Ok(Value::new_none())
            } else {
                decode_json_value(val, inner, path, records, heap)
            }
        }
        WitType::List(inner) => {
            let Some(arr) = val.as_array() else {
                return Err(json_mismatch(path, "array", val));
            };
            let mut out = Vec::with_capacity(arr.len());
            for (idx, item) in arr.iter().enumerate() {
                out.push(decode_json_value(
                    item,
                    inner,
                    &format!("{path}[{idx}]"),
                    records,
                    heap,
                )?);
            }
            Ok(heap.alloc(out))
        }
        WitType::Record(rec_name) => {
            let decl = records.get(rec_name).ok_or_else(|| {
                starlark::Error::new_other(anyhow!("未知返回记录类型 `{rec_name}`"))
            })?;
            let Some(obj) = val.as_object() else {
                return Err(json_mismatch(path, &wit_to_type_name(rec_name), val));
            };
            let mut fields = Vec::with_capacity(decl.fields.len());
            for field in &decl.fields {
                let ident = wit_to_ident(&field.name);
                let decoded = match obj.get(&field.name) {
                    Some(raw) => decode_json_value(
                        raw,
                        &field.ty,
                        &format!("{path}.{ident}"),
                        records,
                        heap,
                    )?,
                    None => {
                        return Err(starlark::Error::new_other(anyhow!(
                            "返回记录 `{path}` 缺少字段 `{}`",
                            field.name
                        )));
                    }
                };
                fields.push((ident, decoded));
            }
            Ok(heap.alloc(AllocStruct(fields)))
        }
        WitType::Handle { .. } => unreachable!("声明加载已拒绝返回句柄"),
    }
}

fn type_mismatch<'v>(param_name: &str, expected: &str, actual: Value<'v>) -> starlark::Error {
    starlark::Error::new_other(anyhow!(
        "参数 `{param_name}` 期望类型 `{expected}`，实际得到 `{}`",
        actual.get_type()
    ))
}

fn json_mismatch(path: &str, expected: &str, actual: &serde_json::Value) -> starlark::Error {
    starlark::Error::new_other(anyhow!(
        "返回值 `{path}` 期望 JSON `{expected}`，实际：{actual}"
    ))
}
