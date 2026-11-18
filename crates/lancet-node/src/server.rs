// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Unix 域套接字服务与每条连接的请求处理。

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use lancet_protocol::{
    Capabilities, CatalogDigest, EnvironmentSummary, Frame, ProtocolVersion, Request, Response,
    ToolSpec, read_frame, write_frame,
};

/// 节点在 `hello_ok` 中声明的结算宽限毫秒数。
pub const SETTLE_GRACE_MS: u64 = 5_000;

/// 连接线程共享的只读节点视图。
pub struct NodeView {
    pub node_id: String,
    pub display_name: String,
    pub capabilities: Capabilities,
    pub catalog_digest: CatalogDigest,
    pub environment_summary: EnvironmentSummary,
    pub tools: Vec<ToolSpec>,
}

/// 绑定 `<state_dir>/node.sock` 并为每条连接分派线程处理请求。
pub fn serve(state_dir: &Path, view: NodeView) -> anyhow::Result<()> {
    let sock_path = state_dir.join("node.sock");
    match std::fs::remove_file(&sock_path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err)
                .with_context(|| format!("无法删除旧的套接字文件：{}", sock_path.display()));
        }
    }
    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("无法绑定 Unix 域套接字：{}", sock_path.display()))?;
    let shared = Arc::new(view);

    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let shared = Arc::clone(&shared);
                if let Err(err) = std::thread::Builder::new().spawn(move || {
                    handle_connection(stream, &shared);
                }) {
                    eprintln!("警告：无法创建连接处理线程：{err}");
                }
            }
            Err(err) => {
                eprintln!("警告：接受套接字连接失败：{err}");
            }
        }
    }
    Ok(())
}

fn handle_connection(mut stream: UnixStream, shared: &NodeView) {
    let Ok(Some(first)) = read_frame::<_, Request>(&mut stream) else {
        return;
    };
    let Request::Hello { supported_versions } = first.message else {
        return;
    };
    if !supported_versions.contains(&ProtocolVersion::V1) {
        return;
    }

    let hello_ok = Frame {
        request_id: first.request_id,
        message: Response::HelloOk {
            version: ProtocolVersion::V1,
            node_id: shared.node_id.clone(),
            display_name: shared.display_name.clone(),
            capabilities: shared.capabilities,
        },
    };
    if write_frame(&mut stream, &hello_ok).is_err() {
        return;
    }

    while let Ok(Some(frame)) = read_frame::<_, Request>(&mut stream) {
        match frame.message {
            Request::Describe => {
                let describe_ok = Frame {
                    request_id: frame.request_id,
                    message: Response::DescribeOk {
                        catalog_digest: shared.catalog_digest.clone(),
                        environment_summary: shared.environment_summary.clone(),
                        tools: shared.tools.clone(),
                    },
                };
                if write_frame(&mut stream, &describe_ok).is_err() {
                    return;
                }
            }
            // 握手之后只处理 describe；其余请求（包括再次 hello）关闭连接，不回复。
            Request::Hello { .. }
            | Request::Execute { .. }
            | Request::Cancel { .. }
            | Request::Receipt { .. } => return,
        }
    }
}
