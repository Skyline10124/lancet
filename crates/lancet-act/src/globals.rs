// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `act` 解释器全局环境构建与扩展函数。

use regex::Regex;
use starlark::environment::{Globals, GlobalsBuilder, LibraryExtension};
use starlark::starlark_module;
use starlark::values::Heap;
use starlark::values::ValueOfUnchecked;
use starlark::values::float::UnpackFloat;
use starlark::values::typing::StarlarkIter;

use crate::text::TextView;

/// 移除反思与元编程全局函数后构造 `act` 使用的纯计算全局环境。
pub fn build_globals() -> Globals {
    let base = Globals::extended_by(&[
        LibraryExtension::Json,
        LibraryExtension::SetType,
        LibraryExtension::StructType,
        LibraryExtension::Map,
        LibraryExtension::Filter,
        LibraryExtension::Print,
    ]);

    let mut builder = GlobalsBuilder::new();
    builder.frozen_heap().add_reference(base.heap());
    for (name, value) in base.iter() {
        if !matches!(name, "dir" | "getattr" | "hasattr" | "type") {
            builder.set(name, value);
        }
    }
    register_core_helpers(&mut builder);
    register_regex(&mut builder);
    builder.build()
}

#[starlark_module]
fn register_core_helpers(builder: &mut GlobalsBuilder) {
    fn sum<'v>(
        xs: ValueOfUnchecked<'v, StarlarkIter<UnpackFloat>>,
        heap: Heap<'v>,
    ) -> starlark::Result<ValueOfUnchecked<'v, UnpackFloat>> {
        let mut acc = heap.alloc(0i32);
        for item in xs.get().iterate(heap)? {
            acc = acc.add(item, heap)?;
        }
        Ok(ValueOfUnchecked::new(acc))
    }

    fn text(s: &str) -> anyhow::Result<TextView> {
        Ok(TextView::new(s))
    }
}

fn register_regex(builder: &mut GlobalsBuilder) {
    #[starlark_module]
    fn regex_members(builder: &mut GlobalsBuilder) {
        fn r#match(pattern: &str, s: &str) -> anyhow::Result<bool> {
            let re = Regex::new(pattern)?;
            Ok(re.find(s).is_some_and(|m| m.start() == 0))
        }

        fn findall(pattern: &str, s: &str) -> anyhow::Result<Vec<String>> {
            let re = Regex::new(pattern)?;
            Ok(re.find_iter(s).map(|m| m.as_str().to_owned()).collect())
        }

        fn replace(pattern: &str, replacement: &str, s: &str) -> anyhow::Result<String> {
            let re = Regex::new(pattern)?;
            Ok(re.replace_all(s, replacement).into_owned())
        }
    }

    builder.namespace("regex", regex_members);
}
