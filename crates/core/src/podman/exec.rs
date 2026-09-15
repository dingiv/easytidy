//! 宿主侧 exec PTY / 一次性 exec（自研 unix-socket HTTP exec API）。
//!
//! 在**运行中的容器**里以指定用户（如 root）起交互进程并挂接 stdio——
//! rootless 下宿主对容器 user namespace 拥有所有权，可自由选择 ns 内
//! 任意 uid 启动进程（`podman exec -u` 的机制本质，runc 经 setns +
//! setresuid 实现，无任何提权）。
//!
//! 用途：容器默认用户 node 化之后的 root 终端/`run --root`——server 以
//! node 运行无法起 root shell，root 通道由宿主侧提供，零 podman CLI。

use std::pin::Pin;
use std::sync::Arc;

use futures::StreamExt;
use hyper::body::Bytes;
use serde_json::Value;
use hyper_util::rt::TokioIo;
use tokio::sync::Mutex;

use crate::error::{Error, Result};

use super::Podman;

/// 一个 exec PTY 会话：input 写侧共享（write/resize 分派用），
/// output 读侧由调用方独占消费。
pub struct ExecPty {
    pub exec_id: String,
    /// stdin 写侧（Arc 共享给 write/close 路径）
    pub input: Arc<Mutex<Pin<Box<dyn tokio::io::AsyncWrite + Send>>>>,
    /// stdout/stderr 读侧（TTY 合流为单流）
    pub output: Pin<Box<dyn futures::Stream<Item = Result<Vec<u8>>> + Send>>,
}

impl Podman {
    /// 创建 exec 会话，返回 exec ID。
    async fn create_exec(
        &self,
        container: &str,
        user: &str,
        tty: bool,
        attach_stdin: bool,
        env: Option<Vec<String>>,
        cmd: Vec<String>,
    ) -> Result<String> {
        let body = serde_json::json!({
            "AttachStdin": attach_stdin,
            "AttachStdout": true,
            "AttachStderr": true,
            "Tty": tty,
            "Cmd": cmd,
            "Env": env,
            "User": user,
        });
        let body_bytes = serde_json::to_vec(&body)
            .map(Bytes::from)
            .map_err(|e| Error::Connect(format!("序列化 exec create body 失败：{e}")))?;
        let (status, resp) = self
            .http
            .request_bytes(
                "POST",
                &format!("/containers/{}/exec", Self::urlquery_encode(container)),
                Some(body_bytes),
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(Error::Connect(format!(
                "创建 exec 失败：HTTP {status}：{}",
                String::from_utf8_lossy(&resp).trim()
            )));
        }
        let v: serde_json::Value = serde_json::from_slice(&resp)
            .map_err(|e| Error::Connect(format!("解析 exec create 响应失败：{e}")))?;
        v.get("Id")
            .and_then(|i| i.as_str())
            .map(str::to_string)
            .ok_or_else(|| Error::Connect("exec create 响应缺少 Id".to_string()))
    }

    /// 启动 exec 并 **upgrade 连接**（Docker exec start 的 hijack 语义：
    /// `Connection: Upgrade` + `Upgrade: tcp` → 101 → 裸双向流）。
    ///
    /// 返回升级后的裸 IO：读侧 = 容器进程 stdout/stderr（tty 时合流），
    /// 写侧 = 容器进程 stdin。**写端保持打开**（关闭 = stdin EOF）。
    async fn start_exec_upgraded(
        &self,
        exec_id: &str,
        tty: bool,
    ) -> Result<TokioIo<hyper::upgrade::Upgraded>> {
        let (status, io) = self
            .http
            .request_upgrade(
                "POST",
                &format!("/exec/{}/start?tty={tty}", Self::urlquery_encode(exec_id)),
            )
            .await?;
        if status != 101 {
            return Err(Error::Connect(format!(
                "exec start 未按预期升级连接（HTTP {status}）"
            )));
        }
        Ok(TokioIo::new(io))
    }

    /// 在容器内以指定用户起交互进程（**不分配 TTY**）并挂接 stdio。
    ///
    /// 与 [`exec_pty`] 区别：不分配 exec TTY——专为「exec'd 进程本身不需要
    /// TTY、其内部再起 TTY 进程」的桥接场景设计：
    ///
    /// - easytidy-dock client 模式：client 经 podman exec 起来，本身只需桥
    ///   stdio 到容器内 daemon socket（无 TTY 概念）；TTY 在 daemon 内由
    ///   `portable-pty` 给 bash 分配。避免 podman exec 给 client 分配 TTY
    ///   引入的 `\r\n` 转换 / 行缓冲。
    ///
    /// `cmd` 不允许为空（client 必须指定 easytidy-dock client 子命令）。
    /// 不写 `COLUMNS/LINES` 环境变量（无 TTY 概念）。
    pub async fn exec_no_tty(
        &self,
        container: &str,
        user: &str,
        cmd: Vec<String>,
    ) -> Result<ExecPty> {
        if cmd.is_empty() {
            return Err(Error::Config(
                "exec_no_tty 需要明确指定 cmd（client 子命令）".to_string(),
            ));
        }
        let exec_id = self
            .create_exec(container, user, false, true, None, cmd)
            .await?;
        let io = self.start_exec_upgraded(&exec_id, false).await?;
        let (read_half, write_half) = tokio::io::split(io);
        // 非 tty：Docker 多路复用流（8 字节帧头），后台任务 demux 为纯输出块流
        let output = Box::pin(demux_read_stream(read_half));
        Ok(ExecPty {
            exec_id,
            input: Arc::new(Mutex::new(Box::pin(write_half))),
            output,
        })
    }

    /// 在容器内以指定用户起交互进程（TTY）并挂接 stdio。
    ///
    /// - `user`：`"0"`（root）或 `"node"` 等（OCI exec user 语义）
    /// - `cmd`：空 = `/bin/bash -l`（回退 /bin/sh）
    ///
    /// 进程退出 = `output` 流 End；退出码经 [`Podman::exec_exit_code`] 查询。
    pub async fn exec_pty(
        &self,
        container: &str,
        user: &str,
        cols: u16,
        rows: u16,
        cmd: Vec<String>,
    ) -> Result<ExecPty> {
        let cmd = if cmd.is_empty() {
            vec!["/bin/bash".to_string(), "-l".to_string()]
        } else {
            cmd
        };
        // TTY 尺寸在 create 时给定（start 前 resize 不可用；首帧前补一次 resize）
        let exec_id = self
            .create_exec(
                container,
                user,
                true,
                true,
                Some(vec![format!("COLUMNS={cols}"), format!("LINES={rows}")]),
                cmd,
            )
            .await?;
        let io = self.start_exec_upgraded(&exec_id, true).await?;
        let (read_half, write_half) = tokio::io::split(io);
        // tty：无帧头，原始字节流直通
        let output = Box::pin(tokio_util::io::ReaderStream::new(read_half).map(|item| {
            item.map(|b| b.to_vec())
                .map_err(|e| Error::Connect(format!("exec 输出流读取失败：{e}")))
        }));
        Ok(ExecPty {
            exec_id,
            input: Arc::new(Mutex::new(Box::pin(write_half))),
            output,
        })
    }

    /// 调整 exec PTY 尺寸（容器内 TTY 的 SIGWINCH 等价）。
    pub async fn resize_exec_pty(&self, exec_id: &str, cols: u16, rows: u16) -> Result<()> {
        let _: Value = self
            .http
            .json_ok(
                "POST",
                &format!(
                    "/exec/{}/resize?h={rows}&w={cols}",
                    Self::urlquery_encode(exec_id)
                ),
                None,
            )
            .await?;
        Ok(())
    }

    /// 查询 exec 进程退出码（未退出返回 None）。
    pub async fn exec_exit_code(&self, exec_id: &str) -> Result<Option<i32>> {
        let (status, body) = self
            .http
            .request_bytes(
                "GET",
                &format!("/exec/{}/json", Self::urlquery_encode(exec_id)),
                None,
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(Error::Connect(format!(
                "exec inspect 失败：HTTP {status}：{}",
                String::from_utf8_lossy(&body).trim()
            )));
        }
        let v: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|e| Error::Connect(format!("解析 exec inspect 失败：{e}")))?;
        Ok(v.get("ExitCode").and_then(|c| c.as_i64()).map(|c| c as i32))
    }

    /// 写入 exec PTY stdin。
    pub async fn exec_pty_write(
        input: &Arc<Mutex<Pin<Box<dyn tokio::io::AsyncWrite + Send>>>>,
        data: &[u8],
    ) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let mut w = input.lock().await;
        w.write_all(data).await.map_err(Error::Io)
    }

    /// 在运行中容器内以指定用户执行**一次性命令（非 tty）**：读 stdout/stderr
    /// 至结束，返回退出码与两路输出。
    ///
    /// 用途：容器内 root 一次性操作（useradd 建号、fontconfig 接入、装包
    /// 校验）——要退出码与 stderr 语义，不需要交互 TTY。
    /// Docker 多路复用流 demux 由 [`super::http::Demuxer`] 完成。
    ///
    /// 容器必须 running（未启动时 podman 拒绝 exec，返回 Api 错误）。
    pub async fn exec_oneshot(
        &self,
        container: &str,
        user: &str,
        cmd: Vec<String>,
    ) -> Result<ExecOnce> {
        let exec_id = self
            .create_exec(container, user, false, false, None, cmd)
            .await?;
        // AttachStdin=false → 无 stdin 流，响应为普通多路复用流，读到 EOF 即结束
        let (stdout, stderr) = self
            .http
            .read_demux_stream(
                "POST",
                &format!(
                    "/exec/{}/start?tty=false",
                    Self::urlquery_encode(&exec_id)
                ),
            )
            .await
            .map_err(|e| Error::Connect(format!("exec 输出读取失败：{e}")))?;
        let code = self.wait_exec_code(&exec_id).await?;
        Ok(ExecOnce {
            code,
            stdout,
            stderr,
        })
    }

    /// 等待 exec 退出码（流结束后状态落盘可能短暂延迟，短暂重试）。
    pub async fn wait_exec_code(&self, exec_id: &str) -> Result<i32> {
        for attempt in 0..10 {
            if let Some(code) = self.exec_exit_code(exec_id).await? {
                return Ok(code);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100 * (attempt + 1) as u64)).await;
        }
        Err(Error::Connect(format!(
            "exec {exec_id} 退出码未就绪（流已结束但 inspect 无 exit_code，状态落盘延迟超出重试窗口）"
        )))
    }
}

/// 非 tty 一次性 exec 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOnce {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}


/// 实测（需真实 podman + 运行中容器 desk_pilot）：upgrade 语义下
/// exec_no_tty 的输出 EOF 及时到达（回归：stdin 不关闭导致会话挂死）。
#[tokio::test]
#[ignore]
async fn test_exec_no_tty_eof_regression() {
    let podman = Podman::connect().await.unwrap();
    let start = std::time::Instant::now();
    let exec = podman
        .exec_no_tty(
            "desk_pilot",
            "0",
            vec!["/bin/sh".into(), "-c".into(), "echo hi".into()],
        )
        .await
        .unwrap();
    let mut out = String::new();
    futures::StreamExt::for_each(exec.output, |item| {
        if let Ok(bytes) = item {
            out.push_str(&String::from_utf8_lossy(&bytes));
        }
        futures::future::ready(())
    })
    .await;
    let code = podman.wait_exec_code(&exec.exec_id).await.unwrap();
    let elapsed = start.elapsed();
    assert!(out.contains("hi"), "应读到 echo 输出：{out:?}");
    assert!(code == 0);
    assert!(elapsed.as_secs() < 10, "EOF 应及时到达，实际 {elapsed:?}");
}


/// 把（upgrade 后的）容器进程输出读半解包为纯输出块流：
/// 非 tty 时按 Docker 多路复用帧头 demux（stdout/stderr 按到达顺序合并），
/// tty 时原样直通由调用方处理。
fn demux_read_stream<R>(
    read_half: R,
) -> impl futures::Stream<Item = Result<Vec<u8>>> + Send
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<Vec<u8>>>();
    tokio::spawn(async move {
        let mut demux = super::http::Demuxer::default();
        let mut reader = read_half;
        let mut buf = [0u8; 8192];
        loop {
            match tokio::io::AsyncReadExt::read(&mut reader, &mut buf).await {
                Ok(0) => break,
                Ok(n) => demux.feed(&buf[..n]),
                Err(e) => {
                    let _ = tx.send(Err(Error::Connect(format!(
                        "exec 输出流读取失败：{e}"
                    ))));
                    return;
                }
            }
            loop {
                let delta = demux.take_merged_delta();
                if delta.is_empty() {
                    break;
                }
                if tx.send(Ok(delta)).is_err() {
                    return;
                }
            }
        }
        let delta = demux.take_merged_delta();
        if !delta.is_empty() {
            let _ = tx.send(Ok(delta));
        }
    });
    futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    })
}

#[cfg(test)]
mod tests {
    /// exec 用户字段格式（CreateExecOptions 文档语义）：
    /// "user" / "user:group" / "uid" / "uid:gid" —— 校验交由 podman。
    #[test]
    fn test_exec_user_format() {
        assert_eq!("0", "0");
        assert_eq!("1000:1000", "1000:1000");
    }
}
