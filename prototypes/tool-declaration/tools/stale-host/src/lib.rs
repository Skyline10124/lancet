//! 按旧宿主接口编译的工具组件，用来观察 `wasmtime` 在实例化时核对导入类型。

wit_bindgen::generate!({ world: "stale", path: "wit", generate_all });

use exports::acme::stale::tool::Guest;
use lancet::host::handles::ReadDir;

struct Stale;

impl Guest for Stale {
    fn peek(root: &ReadDir, path: String) -> Result<Vec<u8>, String> {
        root.read(&path)
    }
}

export!(Stale);
