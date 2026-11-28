//! `free-api-chat --acp`：Agent Client Protocol（ACP）入口，桥接模式。
//!
//! Zed / JetBrains / AWS Quick 等 ACP 客户端把本程序当子进程拉起，经 stdin/stdout 通信。
//! 本进程不打开数据库、不运行引擎，只把 ACP 请求转给正在运行的桌面版（本机通道 API），
//! 再把桌面端的引擎事件转回 ACP；因此不会与桌面版争抢数据库单实例锁。
//!
//! 环境变量：
//! - `SANDWICH_ACP_VERBOSE=1`：把运行日志、状态变化和未映射事件也以思考流转发；
//! - `SANDWICH_ACP_PLAN_MODE`：派任务时的方案模式（缺省沿用通道默认 `cross_review`）；
//! - `SANDWICH_ACP_CHANNEL_URL` / `SANDWICH_ACP_CHANNEL_TOKEN`：覆盖桌面通道地址与令牌。

mod desktop;
mod mapper;
mod server;

use std::io::Write;

type ProtocolOut = blocking::Unblock<Box<dyn Write + Send>>;

/// 协议只能独占 stdout：先复制出真正的 stdout 给协议层，再把 fd 1 指到 stderr，
/// 此后任何 `println!`（含依赖库）都落到 stderr，不会破坏 JSON-RPC 帧。
fn protocol_stdout() -> std::io::Result<ProtocolOut> {
    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;
        // SAFETY: 此时尚未启动运行时线程，复制出的文件描述符由 File 独占。
        let fd = unsafe { libc::fcntl(1, libc::F_DUPFD_CLOEXEC, 3) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let stdout = unsafe { std::fs::File::from_raw_fd(fd) };
        if unsafe { libc::dup2(2, 1) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(blocking::Unblock::new(Box::new(stdout)))
    }
    #[cfg(not(unix))]
    {
        Ok(blocking::Unblock::new(Box::new(std::io::stdout())))
    }
}

pub(crate) use desktop::publish_endpoint;

/// 运行 ACP 入口直到客户端断开；返回进程退出码。
pub fn run() -> i32 {
    let out = match protocol_stdout() {
        Ok(out) => out,
        Err(error) => {
            eprintln!("[acp] 无法设置协议输出：{error}");
            return 1;
        }
    };
    let verbose = matches!(std::env::var("SANDWICH_ACP_VERBOSE").as_deref(), Ok("1"));
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[acp] 无法创建运行时：{error}");
            return 1;
        }
    };
    let bridge = server::Bridge::new(verbose);
    let input = blocking::Unblock::new(std::io::stdin());
    match runtime.block_on(server::serve(out, input, bridge)) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("[acp] 连接结束：{error}");
            1
        }
    }
}
