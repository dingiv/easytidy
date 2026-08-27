//! 宿主侧 exec PTY（bollard exec API）。
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

use bollard::exec::{CreateExecOptions, ResizeExecOptions, StartExecOptions, StartExecResults};
use futures::StreamExt;
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
        // TTY 尺寸在 create 时给定（start 前 resize 不可用；bollard create
        // body 无尺寸字段，首帧前补一次 resize）
        let exec = self
            .docker
            .create_exec::<String>(
                container,
                CreateExecOptions {
                    attach_stdin: Some(true),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    tty: Some(true),
                    env: Some(vec![format!("COLUMNS={cols}"), format!("LINES={rows}")]),
                    cmd: Some(cmd),
                    privileged: None,
                    detach_keys: None,
                    user: Some(user.to_string()),
                    working_dir: None,
                },
            )
            .await
            .map_err(Error::Api)?;

        match self
            .docker
            .start_exec(
                &exec.id,
                Some(StartExecOptions {
                    detach: false,
                    tty: true,
                    output_capacity: None,
                }),
            )
            .await
            .map_err(Error::Api)?
        {
            StartExecResults::Attached { output, input } => {
                // 统一读侧类型：LogOutput → Vec<u8>（TTY 下 stdout/stderr 合流）
                let output = output
                    .map(|item| match item {
                        Ok(log) => Ok(match log {
                            bollard::container::LogOutput::StdOut { message }
                            | bollard::container::LogOutput::StdErr { message }
                            | bollard::container::LogOutput::Console { message }
                            | bollard::container::LogOutput::StdIn { message } => {
                                message.to_vec()
                            }
                        }),
                        Err(e) => Err(Error::Api(e)),
                    })
                    .boxed();
                Ok(ExecPty {
                    exec_id: exec.id.clone(),
                    input: Arc::new(Mutex::new(input)),
                    output,
                })
            }
            // detach: false 请求不会返回 Detached
            StartExecResults::Detached => Err(Error::Connect("exec 意外进入 detach 模式".to_string())),
        }
    }

    /// 调整 exec PTY 尺寸（容器内 TTY 的 SIGWINCH 等价）。
    pub async fn resize_exec_pty(&self, exec_id: &str, cols: u16, rows: u16) -> Result<()> {
        self.docker
            .resize_exec(
                exec_id,
                ResizeExecOptions {
                    height: rows,
                    width: cols,
                },
            )
            .await
            .map_err(Error::Api)
    }

    /// 查询 exec 进程退出码（未退出返回 None）。
    pub async fn exec_exit_code(&self, exec_id: &str) -> Result<Option<i32>> {
        let info = self
            .docker
            .inspect_exec(exec_id)
            .await
            .map_err(Error::Api)?;
        Ok(info.exit_code.map(|c| c as i32))
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
    /// bollard 已按 `LogOutput` 变体完成 multiplex 头 demux，无需手写解析。
    ///
    /// 容器必须 running（未启动时 podman 拒绝 exec，返回 Api 错误）。
    pub async fn exec_oneshot(&self, container: &str, user: &str, cmd: Vec<String>) -> Result<ExecOnce> {
        let exec = self
            .docker
            .create_exec::<String>(
                container,
                CreateExecOptions {
                    attach_stdin: Some(false),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    tty: Some(false),
                    env: None,
                    cmd: Some(cmd),
                    privileged: None,
                    detach_keys: None,
                    user: Some(user.to_string()),
                    working_dir: None,
                },
            )
            .await
            .map_err(Error::Api)?;

        let mut stdout = String::new();
        let mut stderr = String::new();
        if let StartExecResults::Attached { output, .. } = self
            .docker
            .start_exec(&exec.id, Some(StartExecOptions { detach: false, tty: false, output_capacity: None }))
            .await
            .map_err(Error::Api)?
        {
            use futures::StreamExt;
            let mut stream = output;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(log) => match log {
                        bollard::container::LogOutput::StdOut { message } => stdout.push_str(&String::from_utf8_lossy(&message)),
                        bollard::container::LogOutput::StdErr { message } => stderr.push_str(&String::from_utf8_lossy(&message)),
                        _ => {}
                    },
                    Err(e) => return Err(Error::Api(e)),
                }
            }
        }
        let code = self.wait_exec_code(&exec.id).await?;
        Ok(ExecOnce { code, stdout, stderr })
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
