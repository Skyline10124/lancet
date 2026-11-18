// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `TextView` 只读文本视图类型及其行级方法。

use std::fmt::{self, Display, Write as _};

use allocative::Allocative;
use regex::Regex;
use serde::Serialize;
use starlark::any::ProvidesStaticType;
use starlark::environment::{Methods, MethodsBuilder};
use starlark::starlark_module;
use starlark::values::{StarlarkPagable, StarlarkValue, starlark_value};

/// `text(s)` 返回的只读文本视图，按原始行边界存储并序列化为 JSON 字符串。
#[derive(Debug, Clone, ProvidesStaticType, StarlarkPagable, Allocative)]
pub struct TextView(Vec<String>);

impl TextView {
    /// 拆行一次并包装原始文本为 `TextView`。
    pub fn new(text: &str) -> Self {
        Self(split_lines_with_endings(text))
    }
}

impl Display for TextView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for line in &self.0 {
            f.write_str(line)?;
        }
        Ok(())
    }
}

impl Serialize for TextView {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let joined: String = self.0.concat();
        serializer.serialize_str(&joined)
    }
}

starlark::starlark_simple_value!(TextView);

#[starlark_value(type = "TextView")]
impl<'v> StarlarkValue<'v> for TextView {
    fn get_methods() -> Option<&'static Methods> {
        Some(TEXT_VIEW_METHODS.methods())
    }

    fn collect_repr(&self, collector: &mut String) {
        let joined: String = self.0.concat();
        let _ = write!(collector, "text({joined:?})");
    }
}

starlark::methods_static!(TEXT_VIEW_METHODS = text_view_methods);

#[starlark_module]
fn text_view_methods(builder: &mut MethodsBuilder) {
    fn grep<'v>(
        this: &TextView,
        pattern: &str,
        #[starlark(default = 0)] before: usize,
        #[starlark(default = 0)] after: usize,
    ) -> anyhow::Result<TextView> {
        let re = Regex::new(pattern)?;
        if this.0.is_empty() {
            return Ok(TextView(Vec::new()));
        }

        let mut selected = vec![false; this.0.len()];
        for (idx, line) in this.0.iter().enumerate() {
            if re.is_match(strip_line_ending(line)) {
                let start = idx.saturating_sub(before);
                let end = idx.saturating_add(after).min(this.0.len() - 1);
                for flag in &mut selected[start..=end] {
                    *flag = true;
                }
            }
        }

        let out = this
            .0
            .iter()
            .zip(selected)
            .filter(|&(_, keep)| keep)
            .map(|(line, _)| line.clone())
            .collect();
        Ok(TextView(out))
    }

    fn head<'v>(this: &TextView, n: usize) -> anyhow::Result<TextView> {
        Ok(TextView(this.0.iter().take(n).cloned().collect()))
    }

    fn tail<'v>(this: &TextView, n: usize) -> anyhow::Result<TextView> {
        let skip = this.0.len().saturating_sub(n);
        Ok(TextView(this.0.iter().skip(skip).cloned().collect()))
    }

    fn splitlines<'v>(
        this: &TextView,
        #[starlark(require = pos, default = false)] keepends: bool,
    ) -> anyhow::Result<Vec<String>> {
        let out = this
            .0
            .iter()
            .map(|line| {
                if keepends {
                    line.clone()
                } else {
                    strip_line_ending(line).to_owned()
                }
            })
            .collect();
        Ok(out)
    }
}

fn strip_line_ending(line: &str) -> &str {
    if let Some(stripped) = line.strip_suffix("\r\n") {
        stripped
    } else if let Some(stripped) = line.strip_suffix(['\n', '\r']) {
        stripped
    } else {
        line
    }
}

/// 按 Starlark `str.splitlines()` 相同的规则（`\r\n`、`\n`、`\r`）切分文本行并保留行结束符。
fn split_lines_with_endings(mut s: &str) -> Vec<String> {
    let mut lines = Vec::new();
    while !s.is_empty() {
        if let Some(pos) = s.find(['\n', '\r']) {
            let end = if s.as_bytes().get(pos..pos + 2) == Some(b"\r\n") {
                pos + 2
            } else {
                pos + 1
            };
            lines.push(s[..end].to_owned());
            s = &s[end..];
        } else {
            lines.push(s.to_owned());
            break;
        }
    }
    lines
}
