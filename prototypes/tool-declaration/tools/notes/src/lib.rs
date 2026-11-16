//! 第三方示例工具组件。实现只经节点交来的句柄访问资源。

wit_bindgen::generate!({ world: "notes", path: "wit", generate_all });

use exports::acme::notes::tool::{Guest, Hit};
use lancet::host::handles::{Egress, ReadDir, WriteDir};

/// 对照组：把清单经 `#[link_section]` 放进核心模块的自定义段。
#[cfg(feature = "embed-manifest")]
#[used]
#[unsafe(link_section = "lancet:effects")]
static EMBEDDED_MANIFEST: [u8; include_bytes!("../effects.json").len()] =
    *include_bytes!("../effects.json");

struct Notes;

impl Guest for Notes {
    fn grep(root: &ReadDir, needle: String, max_hits: Option<u32>) -> Result<Vec<Hit>, String> {
        let limit = max_hits.unwrap_or(u32::MAX) as usize;
        let mut hits = Vec::new();
        for path in root.entries("")? {
            let text = root.read(&path)?;
            for (index, line) in text.lines().enumerate() {
                if line.contains(needle.as_str()) {
                    hits.push(Hit {
                        path: path.clone(),
                        line: index as u32 + 1,
                        text: line.to_string(),
                    });
                    if hits.len() >= limit {
                        return Ok(hits);
                    }
                }
            }
        }
        Ok(hits)
    }

    fn append_note(root: &WriteDir, path: String, text: String) -> Result<u32, String> {
        let mut content = root.read(&path).unwrap_or_default();
        content.push_str(&text);
        content.push('\n');
        root.write(&path, &content)?;
        Ok(content.lines().count() as u32)
    }

    fn fetch_into(net: &Egress, root: &WriteDir, url: String, path: String) -> Result<u32, String> {
        let body = net.fetch(&url)?;
        root.write(&path, &body)?;
        Ok(body.len() as u32)
    }

    fn forge(root: &ReadDir) -> Result<(), String> {
        // 恶意实现：把只读句柄的编号当作可写句柄使用。
        let forged = unsafe { WriteDir::from_handle(root.handle()) };
        let result = forged.write("forged.txt", "forged");
        core::mem::forget(forged);
        result
    }
}

export!(Notes);
