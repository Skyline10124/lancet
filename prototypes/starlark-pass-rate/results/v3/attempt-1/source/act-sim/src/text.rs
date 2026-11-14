use allocative::Allocative;
use starlark::environment::{Methods, MethodsBuilder};
use starlark::values::{
    Heap, NoSerialize, ProvidesStaticType, StarlarkPagable, StarlarkValue, Value, ValueLike,
};
use starlark_derive::{starlark_module, starlark_value};
use std::fmt;

#[derive(Debug, ProvidesStaticType, NoSerialize, StarlarkPagable, Allocative)]
pub struct Text(pub String);
starlark::starlark_simple_value!(Text);
impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
starlark::methods_static!(TEXT_METHODS = methods);
#[starlark_value(type = "text_view")]
impl<'v> StarlarkValue<'v> for Text {
    fn get_methods() -> Option<&'static Methods> {
        Some(TEXT_METHODS.methods())
    }
}
fn contents<'v>(value: Value<'v>) -> anyhow::Result<&'v str> {
    Ok(&value
        .downcast_ref::<Text>()
        .ok_or_else(|| anyhow::anyhow!("需要文本视图"))?
        .0)
}
fn selected(lines: Vec<&str>) -> Text {
    Text(lines.join("\n"))
}
#[starlark_module]
fn methods(builder: &mut MethodsBuilder) {
    fn grep<'v>(
        #[starlark(this)] this: Value<'v>,
        pattern: &str,
        #[starlark(default = 0)] before: usize,
        #[starlark(default = 0)] after: usize,
        heap: Heap<'v>,
    ) -> anyhow::Result<Value<'v>> {
        let re = regex::Regex::new(pattern)?;
        let lines: Vec<_> = contents(this)?.lines().collect();
        let mut keep = vec![false; lines.len()];
        for (i, line) in lines.iter().enumerate() {
            if re.is_match(line) {
                for flag in &mut keep[i.saturating_sub(before)
                    ..(i.saturating_add(after).saturating_add(1)).min(lines.len())]
                {
                    *flag = true;
                }
            }
        }
        Ok(heap.alloc(selected(
            lines
                .into_iter()
                .zip(keep)
                .filter_map(|(s, k)| k.then_some(s))
                .collect(),
        )))
    }
    fn head<'v>(
        #[starlark(this)] this: Value<'v>,
        n: usize,
        heap: Heap<'v>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(heap.alloc(selected(contents(this)?.lines().take(n).collect())))
    }
    fn tail<'v>(
        #[starlark(this)] this: Value<'v>,
        n: usize,
        heap: Heap<'v>,
    ) -> anyhow::Result<Value<'v>> {
        let lines: Vec<_> = contents(this)?.lines().collect();
        Ok(heap.alloc(selected(lines[lines.len().saturating_sub(n)..].to_vec())))
    }
    fn lines(#[starlark(this)] this: Value) -> anyhow::Result<Vec<String>> {
        Ok(contents(this)?.lines().map(str::to_owned).collect())
    }
}
