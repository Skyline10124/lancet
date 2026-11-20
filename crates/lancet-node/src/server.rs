// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Unix 域套接字服务、连接内多路复用分派与断连取消。

use std::io;
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Context as _;
use lancet_node::act::ActRuntime;
use lancet_node::dispatch::Dispatcher;
use lancet_protocol::{
    Capabilities, CatalogDigest, EnvironmentSummary, Frame, ProtocolVersion, ReceiptLookup,
    Request, Response, ToolSpec, read_frame, write_frame,
};

use crate::calls::Ledger;
use crate::execute;

/// 节点在 `hello_ok` 中声明的结算宽限毫秒数。
pub const SETTLE_GRACE_MS: u64 = 5_000;

/// 连接线程共享的节点服务状态。
pub struct NodeView {
    pub node_id: String,
    pub display_name: String,
    pub capabilities: Capabilities,
    pub catalog_digest: CatalogDigest,
    pub environment_summary: EnvironmentSummary,
    pub tools: Vec<ToolSpec>,
    pub runtime: ActRuntime,
    pub dispatcher: Dispatcher,
    pub ledger: Ledger,
}

/// 绑定 `<state_dir>/node.sock` 并为每条连接分派读线程处理请求。
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
    let mut next_conn_id = 1u64;

    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let conn_id = next_conn_id;
                next_conn_id += 1;
                let shared = Arc::clone(&shared);
                if let Err(err) = std::thread::Builder::new().spawn(move || {
                    handle_connection(stream, conn_id, &shared);
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

fn handle_connection(mut stream: UnixStream, conn_id: u64, shared: &Arc<NodeView>) {
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

    let Ok(writer_stream) = stream.try_clone() else {
        return;
    };
    let writer = Arc::new(Mutex::new(writer_stream));

    while let Ok(Some(frame)) = read_frame::<_, Request>(&mut stream) {
        let received_at = Instant::now();
        if !dispatch_request(frame, received_at, conn_id, shared, &writer) {
            break;
        }
    }

    shared.ledger.cancel_connection(conn_id);
    // 立即关闭底层双向套接字，避免执行线程尚持有写端克隆时读端未及时感知断开。
    let _ = stream.shutdown(Shutdown::Both);
}

fn dispatch_request(
    frame: Frame<Request>,
    received_at: Instant,
    conn_id: u64,
    shared: &Arc<NodeView>,
    writer: &Arc<Mutex<UnixStream>>,
) -> bool {
    match frame.message {
        // 握手后再次收到 hello：关闭连接，不回复。
        Request::Hello { .. } => false,
        Request::Describe => write_reply(
            writer,
            frame.request_id,
            Response::DescribeOk {
                catalog_digest: shared.catalog_digest.clone(),
                environment_summary: shared.environment_summary.clone(),
                tools: shared.tools.clone(),
            },
        ),
        Request::Cancel { invocation_id } => {
            shared.ledger.cancel(&invocation_id);
            write_reply(writer, frame.request_id, Response::CancelOk)
        }
        Request::Receipt { invocation_id } => {
            let lookup = match shared.ledger.query_receipt(&invocation_id) {
                ReceiptLookup::Settled { receipt } => ReceiptLookup::Settled {
                    receipt: receipt.truncate_to_frame(),
                },
                other => other,
            };
            write_reply(writer, frame.request_id, Response::ReceiptResult { lookup })
        }
        Request::Execute {
            invocation_id,
            catalog_digest,
            tool,
            arguments,
            remaining_ms,
            output_limit,
            labels,
        } => {
            let has_writable = shared.dispatcher.has_writable_dirs();
            let validation = execute::validate_execute(
                &shared.catalog_digest,
                &catalog_digest,
                &tool,
                &arguments,
                output_limit,
                has_writable,
            );
            match shared.ledger.admit_execute(
                conn_id,
                invocation_id,
                catalog_digest,
                tool,
                labels,
                has_writable,
                validation,
            ) {
                Ok(admitted) => {
                    let deadline = execute::compute_deadline(received_at, remaining_ms);
                    let shared_bg = Arc::clone(shared);
                    let writer_bg = Arc::clone(writer);
                    let request_id = frame.request_id;
                    if let Err(err) = std::thread::Builder::new()
                        .name(format!("act-exec-{conn_id}-{request_id}"))
                        .spawn(move || {
                            execute::run_admitted(
                                admitted,
                                output_limit,
                                deadline,
                                &shared_bg.runtime,
                                &shared_bg.dispatcher,
                                &shared_bg.ledger,
                                &writer_bg,
                                request_id,
                            );
                        })
                    {
                        crate::calls::fatal_error(
                            anyhow::Error::new(err).context("无法创建执行线程"),
                        );
                    }
                    true
                }
                Err(rejected) => write_reply(
                    writer,
                    frame.request_id,
                    Response::Rejected {
                        code: rejected.code,
                        message: rejected.message,
                    },
                ),
            }
        }
    }
}

fn write_reply(writer: &Mutex<UnixStream>, request_id: u64, message: Response) -> bool {
    let mut guard = writer.lock().expect("套接字写锁不应中毒");
    write_frame(
        &mut *guard,
        &Frame {
            request_id,
            message,
        },
    )
    .is_ok()
}
