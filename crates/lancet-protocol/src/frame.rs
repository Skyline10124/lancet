// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 字节流帧编解码与有界序列化辅助。

use std::fmt;
use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// 单帧 JSON 负载最大字节数（8 MiB，不含 4 字节大端长度前缀）。
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// 协议线缆帧：连接内唯一的 `request_id` 与消息体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame<T> {
    pub request_id: u64,
    pub message: T,
}

/// 帧编解码错误。
#[derive(Debug)]
pub enum FrameError {
    /// 底层 I/O 错误。
    Io(io::Error),
    /// 长度前缀未读满 4 字节即遇到 EOF。
    TruncatedPrefix { bytes_read: usize },
    /// 负载未按声明长度读完即遇到 EOF。
    TruncatedPayload { expected: usize, actual: usize },
    /// 帧长度为 0，无法承载有效的 JSON 负载。
    EmptyFrame,
    /// 帧负载超过 [`MAX_FRAME_BYTES`] 上限。
    ///
    /// 解码时为前缀声明的字节数；编码时为避免构建超大缓冲区而在超限处即刻中止，故为 `None`。
    FrameTooLarge { length: Option<usize>, max: usize },
    /// 帧负载不是有效的 UTF-8 字节序列。
    InvalidUtf8(std::str::Utf8Error),
    /// 帧负载不是合法的协议 JSON。
    InvalidJson(serde_json::Error),
    /// 序列化消息时出错。
    Serialize(serde_json::Error),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "帧读写 I/O 错误：{err}"),
            Self::TruncatedPrefix { bytes_read } => {
                write!(f, "帧长度前缀被截断：仅读取 {bytes_read} 字节，期望 4 字节")
            }
            Self::TruncatedPayload { expected, actual } => {
                write!(
                    f,
                    "帧负载被截断：仅读取 {actual} 字节，期望 {expected} 字节"
                )
            }
            Self::EmptyFrame => write!(f, "帧长度为 0，缺少 JSON 负载"),
            Self::FrameTooLarge {
                length: Some(length),
                max,
            } => write!(f, "帧负载长度 {length} 字节超过单帧上限 {max} 字节"),
            Self::FrameTooLarge { length: None, max } => {
                write!(f, "待编码的帧负载超过单帧上限 {max} 字节")
            }
            Self::InvalidUtf8(err) => write!(f, "帧负载不是有效 UTF-8：{err}"),
            Self::InvalidJson(err) => write!(f, "帧负载 JSON 解析失败：{err}"),
            Self::Serialize(err) => write!(f, "帧负载 JSON 序列化失败：{err}"),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::InvalidUtf8(err) => Some(err),
            Self::InvalidJson(err) | Self::Serialize(err) => Some(err),
            Self::TruncatedPrefix { .. }
            | Self::TruncatedPayload { .. }
            | Self::EmptyFrame
            | Self::FrameTooLarge { .. } => None,
        }
    }
}

/// 仅统计紧凑 JSON 字节数而不分配输出缓冲区的有界 writer；超出 `limit` 即中止写入。
struct BoundedCounter {
    count: usize,
    limit: usize,
    exceeded: bool,
}

impl Write for BoundedCounter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.limit.saturating_sub(self.count) {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "序列化输出超过字节上限",
            ));
        }
        self.count += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 测量紧凑 JSON 序列化长度；超过 `limit` 时提前中止并返回 `Ok(None)`。
pub(crate) fn json_len_within<T: Serialize>(
    value: &T,
    limit: usize,
) -> Result<Option<usize>, serde_json::Error> {
    let mut counter = BoundedCounter {
        count: 0,
        limit,
        exceeded: false,
    };
    match serde_json::to_writer(&mut counter, value) {
        Ok(()) => Ok(Some(counter.count)),
        Err(_) if counter.exceeded => Ok(None),
        Err(err) => Err(err),
    }
}

/// 在 `MAX_FRAME_BYTES` 内缓冲编码帧，避免对超大输入先分配任意大小的字节数组。
struct BoundedFrameBuffer {
    buf: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl BoundedFrameBuffer {
    fn new(limit: usize) -> Self {
        // 头部 4 字节留作大端长度前缀，写入前直接回填。
        Self {
            buf: vec![0u8; 4],
            limit,
            exceeded: false,
        }
    }

    fn payload_len(&self) -> usize {
        self.buf.len() - 4
    }
}

impl Write for BoundedFrameBuffer {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let current = self.payload_len();
        if data.len() > self.limit.saturating_sub(current) {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "帧负载超过单帧上限",
            ));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 从字节流读取并解码一帧；在帧边界遇到正常 EOF 时返回 `Ok(None)`。
pub fn read_frame<R: Read, T: DeserializeOwned>(
    reader: &mut R,
) -> Result<Option<Frame<T>>, FrameError> {
    let mut prefix = [0u8; 4];
    let mut prefix_read = 0;
    while prefix_read < 4 {
        match reader.read(&mut prefix[prefix_read..]) {
            Ok(0) => {
                if prefix_read == 0 {
                    return Ok(None);
                }
                return Err(FrameError::TruncatedPrefix {
                    bytes_read: prefix_read,
                });
            }
            Ok(n) => prefix_read += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(FrameError::Io(err)),
        }
    }

    let length = u32::from_be_bytes(prefix) as usize;
    // 必须先判断单帧上限，再分配和读取正文，防止恶意前缀耗尽内存。
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::FrameTooLarge {
            length: Some(length),
            max: MAX_FRAME_BYTES,
        });
    }
    if length == 0 {
        return Err(FrameError::EmptyFrame);
    }

    let mut payload = vec![0u8; length];
    let mut offset = 0;
    while offset < length {
        match reader.read(&mut payload[offset..]) {
            Ok(0) => {
                return Err(FrameError::TruncatedPayload {
                    expected: length,
                    actual: offset,
                });
            }
            Ok(n) => offset += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(FrameError::Io(err)),
        }
    }

    let text = std::str::from_utf8(&payload).map_err(FrameError::InvalidUtf8)?;
    let frame = serde_json::from_str::<Frame<T>>(text).map_err(FrameError::InvalidJson)?;
    Ok(Some(frame))
}

/// 将一帧编码并写入字节流，负载超过 [`MAX_FRAME_BYTES`] 时拒绝且不触碰底层 `writer`。
pub fn write_frame<W: Write, T: Serialize>(
    writer: &mut W,
    frame: &Frame<T>,
) -> Result<(), FrameError> {
    let mut buffer = BoundedFrameBuffer::new(MAX_FRAME_BYTES);
    if let Err(err) = serde_json::to_writer(&mut buffer, frame) {
        if buffer.exceeded {
            return Err(FrameError::FrameTooLarge {
                length: None,
                max: MAX_FRAME_BYTES,
            });
        }
        return Err(FrameError::Serialize(err));
    }

    let payload_len = buffer.payload_len();
    if payload_len == 0 {
        return Err(FrameError::EmptyFrame);
    }
    let len_u32 = u32::try_from(payload_len).map_err(|_| FrameError::FrameTooLarge {
        length: Some(payload_len),
        max: MAX_FRAME_BYTES,
    })?;
    buffer.buf[0..4].copy_from_slice(&len_u32.to_be_bytes());
    writer.write_all(&buffer.buf).map_err(FrameError::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Capabilities, ProtocolVersion, Request, Response};
    use std::collections::BTreeMap;
    use std::io::Cursor;

    struct FragmentedReader<'a> {
        data: &'a [u8],
        pos: usize,
        chunk: usize,
        inject_eintr_every: usize,
        reads: usize,
    }

    impl<'a> FragmentedReader<'a> {
        fn new(data: &'a [u8], chunk: usize, inject_eintr_every: usize) -> Self {
            Self {
                data,
                pos: 0,
                chunk,
                inject_eintr_every,
                reads: 0,
            }
        }
    }

    impl Read for FragmentedReader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            if self.inject_eintr_every > 0 && self.reads.is_multiple_of(self.inject_eintr_every) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "模拟中断"));
            }
            if self.pos >= self.data.len() {
                return Ok(0);
            }
            let n = (self.data.len() - self.pos).min(self.chunk).min(buf.len());
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    struct PrefixOnlyReader {
        prefix: [u8; 4],
        pos: usize,
    }

    impl Read for PrefixOnlyReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pos >= 4 {
                panic!("超限帧不允许读取正文字节");
            }
            let n = (4 - self.pos).min(buf.len());
            buf[..n].copy_from_slice(&self.prefix[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn roundtrip_request_response_with_chinese_and_big_endian_prefix() {
        let mut labels = BTreeMap::new();
        labels.insert("会话".to_owned(), serde_json::json!("轮次-1"));
        let req_frame = Frame {
            request_id: 7,
            message: Request::Execute {
                invocation_id: "inv-中文-1".to_owned(),
                catalog_digest: crate::digest::CatalogDigest::from_sha256_bytes([0xab; 32]),
                tool: "act".to_owned(),
                arguments: serde_json::json!({"说明": "测试内容"}),
                remaining_ms: 5000,
                output_limit: 65536,
                labels,
            },
        };
        let mut wire = Vec::new();
        write_frame(&mut wire, &req_frame).expect("编码请求帧应当成功");
        let declared_len = u32::from_be_bytes(wire[0..4].try_into().unwrap()) as usize;
        assert_eq!(
            declared_len,
            wire.len() - 4,
            "长度前缀应为大端序且不含自身 4 字节"
        );

        let mut reader = Cursor::new(&wire);
        let decoded: Frame<Request> = read_frame(&mut reader)
            .expect("解码请求帧应当成功")
            .expect("不应在首帧处遇到 EOF");
        assert_eq!(decoded, req_frame, "请求帧中文内容往返应保持一致");

        let resp_frame = Frame {
            request_id: 7,
            message: Response::HelloOk {
                version: ProtocolVersion::V1,
                node_id: "550e8400-e29b-41d4-a716-446655440000".to_owned(),
                display_name: "本地工作区节点".to_owned(),
                capabilities: Capabilities {
                    settle_grace_ms: 2000,
                    receipt_retention_ms: 86_400_000,
                },
            },
        };
        wire.clear();
        write_frame(&mut wire, &resp_frame).expect("编码回复帧应当成功");
        let decoded_resp: Frame<Response> = read_frame(&mut Cursor::new(&wire))
            .expect("解码回复帧应当成功")
            .expect("不应遇到 EOF");
        assert_eq!(decoded_resp, resp_frame, "回复帧中文内容往返应保持一致");
    }

    #[test]
    fn fragmented_reads_and_consecutive_frames_then_clean_eof() {
        let f1 = Frame {
            request_id: 1,
            message: Request::Hello {
                supported_versions: vec![ProtocolVersion::V1, ProtocolVersion::new(99)],
            },
        };
        let f2 = Frame {
            request_id: 2,
            message: Request::Describe,
        };
        let mut wire = Vec::new();
        write_frame(&mut wire, &f1).expect("第一帧编码失败");
        write_frame(&mut wire, &f2).expect("第二帧编码失败");

        let mut reader = FragmentedReader::new(&wire, 2, 3);
        let r1: Frame<Request> = read_frame(&mut reader)
            .expect("分片读取第一帧失败")
            .expect("应读出第一帧");
        let r2: Frame<Request> = read_frame(&mut reader)
            .expect("分片读取第二帧失败")
            .expect("应读出第二帧");
        let eof: Option<Frame<Request>> = read_frame(&mut reader).expect("帧边界 EOF 不应报错");
        assert_eq!(r1, f1, "第一帧应完整还原");
        assert_eq!(r2, f2, "第二帧应完整还原");
        assert!(eof.is_none(), "连续帧之后的正常 EOF 应返回 None");
    }

    #[test]
    fn oversized_prefix_reads_only_four_bytes_without_touching_payload() {
        let prefix = ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes();
        let mut reader = PrefixOnlyReader { prefix, pos: 0 };
        let err = read_frame::<_, Request>(&mut reader).expect_err("超限前缀应直接返回错误");
        assert!(
            matches!(
                err,
                FrameError::FrameTooLarge {
                    length: Some(length),
                    max
                } if length == MAX_FRAME_BYTES + 1 && max == MAX_FRAME_BYTES
            ),
            "应返回 FrameTooLarge，实际为：{err:?}"
        );
        assert_eq!(reader.pos, 4, "仅应读取 4 字节前缀");
    }

    #[test]
    fn frame_size_exact_limit_and_over_limit_encode_decode() {
        let probe = Frame {
            request_id: 1,
            message: Request::Cancel {
                invocation_id: String::new(),
            },
        };
        let mut base = Vec::new();
        write_frame(&mut base, &probe).expect("空字符串探测帧编码失败");
        let base_payload_len = base.len() - 4;
        let fill_len = MAX_FRAME_BYTES - base_payload_len;

        let exact = Frame {
            request_id: 1,
            message: Request::Cancel {
                invocation_id: "a".repeat(fill_len),
            },
        };
        let mut exact_wire = Vec::new();
        write_frame(&mut exact_wire, &exact).expect("恰好达到 8 MiB 上限的帧应允许编码");
        assert_eq!(
            exact_wire.len() - 4,
            MAX_FRAME_BYTES,
            "载荷长度应恰好等于上限"
        );
        let decoded: Frame<Request> = read_frame(&mut Cursor::new(&exact_wire))
            .expect("恰好达到上限的帧应允许解码")
            .expect("应读出帧");
        assert_eq!(decoded, exact, "边界大小帧往返应一致");

        let over = Frame {
            request_id: 1,
            message: Request::Cancel {
                invocation_id: "a".repeat(fill_len + 1),
            },
        };
        let mut sink = Vec::new();
        let err = write_frame(&mut sink, &over).expect_err("超出 8 MiB 应拒绝编码");
        assert!(
            matches!(err, FrameError::FrameTooLarge { max, .. } if max == MAX_FRAME_BYTES),
            "编码超限应返回 FrameTooLarge，实际为：{err:?}"
        );
        assert!(sink.is_empty(), "编码超限时不应向底层写入任何字节");
    }

    #[test]
    fn distinguishes_truncation_invalid_utf8_and_invalid_json() {
        let err_prefix = read_frame::<_, Request>(&mut Cursor::new(&[0u8, 0, 1]))
            .expect_err("不足 4 字节前缀应视为截断");
        assert!(
            matches!(err_prefix, FrameError::TruncatedPrefix { bytes_read: 3 }),
            "应返回 TruncatedPrefix，实际为：{err_prefix:?}"
        );

        let mut truncated_body = Vec::new();
        truncated_body.extend_from_slice(&10u32.to_be_bytes());
        truncated_body.extend_from_slice(b"{\"a\":1}");
        let err_body = read_frame::<_, Request>(&mut Cursor::new(&truncated_body))
            .expect_err("正文不足声明长度应视为截断");
        assert!(
            matches!(
                err_body,
                FrameError::TruncatedPayload {
                    expected: 10,
                    actual: 7
                }
            ),
            "应返回 TruncatedPayload，实际为：{err_body:?}"
        );

        let err_empty = read_frame::<_, Request>(&mut Cursor::new(&0u32.to_be_bytes()))
            .expect_err("0 字节帧应被拒绝");
        assert!(
            matches!(err_empty, FrameError::EmptyFrame),
            "应返回 EmptyFrame，实际为：{err_empty:?}"
        );

        let mut bad_utf8 = Vec::new();
        bad_utf8.extend_from_slice(&2u32.to_be_bytes());
        bad_utf8.extend_from_slice(&[0xff, 0xfe]);
        let err_utf8 =
            read_frame::<_, Request>(&mut Cursor::new(&bad_utf8)).expect_err("非法 UTF-8 应被识别");
        assert!(
            matches!(err_utf8, FrameError::InvalidUtf8(_)),
            "应返回 InvalidUtf8，实际为：{err_utf8:?}"
        );

        let mut bad_json = Vec::new();
        let payload = b"{not json}";
        bad_json.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bad_json.extend_from_slice(payload);
        let err_json =
            read_frame::<_, Request>(&mut Cursor::new(&bad_json)).expect_err("非法 JSON 应被识别");
        assert!(
            matches!(err_json, FrameError::InvalidJson(_)),
            "应返回 InvalidJson，实际为：{err_json:?}"
        );
    }
}
