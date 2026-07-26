use std::io::{Read, Write};
use std::sync::Arc;
use std::thread;

use anyhow::{Context as _, Result};
use async_channel::Receiver;
use parking_lot::Mutex;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

pub(crate) type SharedPtyWriter = Arc<Mutex<Box<dyn Write + Send>>>;

const DEFAULT_CHILD_TERM: &str = "xterm-256color";

/// PTY 输出通道能缓冲多少个读块（每块最多 8 KB），即最多约 8 MB。
///
/// 定得足够大，让正常的突发输出不至于把 reader 线程按住；又足够小，让洪泛场景下
/// 内存有上界。
const PTY_OUTPUT_CHANNEL_CAPACITY: usize = 1024;

/// 待写队列深度。足够吸收大粘贴与突发按键，又不会在写侧长期堵住时无限堆积。
const PTY_WRITE_QUEUE_CAPACITY: usize = 4096;

pub(crate) struct PtySession {
    pub(crate) master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    pub(crate) writer: SharedPtyWriter,
    pub(crate) child: Arc<Mutex<Box<dyn Child + Send>>>,
    pub(crate) output_rx: Receiver<Vec<u8>>,
    pub(crate) shell: String,
}

impl PtySession {
    pub(crate) fn spawn(rows: u16, cols: u16, pixel_width: u16, pixel_height: u16) -> Result<Self> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let system = native_pty_system();
        let pair = system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width,
                pixel_height,
            })
            .context("failed to create PTY")?;

        let mut command = CommandBuilder::new(shell.clone());
        command.arg("-i");
        command.env("TERM", child_term());
        command.env("COLORTERM", "truecolor");
        command.env("TERM_PROGRAM", "rterminal");
        command.env_remove("NO_COLOR");

        let child = pair
            .slave
            .spawn_command(command)
            .context("failed to spawn shell")?;

        let master = Arc::new(Mutex::new(pair.master));
        let writer = {
            let writer = master
                .lock()
                .take_writer()
                .context("failed to get PTY writer")?;
            Arc::new(Mutex::new(writer))
        };

        let mut reader = master
            .lock()
            .try_clone_reader()
            .context("failed to clone PTY reader")?;
        // 有界通道，恢复背压。用 unbounded 时 reader 线程永远抢着读，子进程侧感觉不到
        // 任何阻力，而 UI 每次 update 只消费 ≤256 KB —— `cat 大文件` / `yes` 洪泛时
        // 通道会无限堆内存。
        //
        // 有界之后 send_blocking 会在满时阻塞 reader 线程，内核 PTY 缓冲随之填满，
        // 子进程最终在 write 上阻塞，这正是终端该有的行为。
        // 上限 = PTY_OUTPUT_CHANNEL_CAPACITY × 8 KB 读缓冲。
        let (tx, rx) = async_channel::bounded(PTY_OUTPUT_CHANNEL_CAPACITY);
        thread::spawn(move || {
            let mut buf = vec![0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    // 真正的 EOF：子进程关掉了它那一端。
                    Ok(0) => break,
                    Ok(read) => {
                        if tx.send_blocking(buf[..read].to_vec()).is_err() {
                            // 接收端已经没了（tab 关闭），正常收摊。
                            break;
                        }
                    }
                    // `Read::read` 不会自己重试 EINTR。原来这里和其它错误一样直接 break，
                    // 于是一次信号打断就会让 reader 线程退出、channel 关闭，而 pump 任务
                    // 把 channel 关闭当成 "shell exited" —— shell 其实还活着，终端却
                    // 永久失去了输出。
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(err) => {
                        // 原来是 `Err(_) => break`：PTY 异常断开完全静默，没法排障。
                        eprintln!("pty reader stopped: {err}");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            master,
            writer,
            child: Arc::new(Mutex::new(child)),
            output_rx: rx,
            shell,
        })
    }
}

pub(crate) fn write_to_pty(writer: &SharedPtyWriter, bytes: &[u8]) -> Result<()> {
    let mut writer = writer.lock();
    writer
        .write_all(bytes)
        .context("failed to write bytes to PTY")?;
    writer.flush().context("failed to flush PTY writer")?;
    Ok(())
}

/// 往 PTY 投递字节的句柄。
///
/// 为什么不直接调 `write_to_pty`：那是阻塞的 `write_all + flush`，而调用方
/// （键盘、DSR 应答、debug 接口）都在 UI 线程上。前台程序不读 stdin 时
/// （被 Ctrl-S 流控、或正在 sleep），一旦粘贴的内容超过内核 PTY 缓冲（约 16–64 KB），
/// `write_all` 会无限期阻塞，整个窗口失去响应。
///
/// 所以真实写入交给一个专职线程，UI 侧只投递。通道是 FIFO，字节顺序不变 ——
/// 这对转义序列（以及 DSR 应答相对普通输入的位置）是必须的。
#[derive(Clone)]
pub(crate) struct PtyWriteHandle {
    sender: async_channel::Sender<Vec<u8>>,
}

impl PtyWriteHandle {
    /// 起一个专职写线程，返回投递句柄。
    pub(crate) fn spawn(writer: SharedPtyWriter) -> Self {
        // 有界：写侧堵住时不要让待写数据无限堆积。满了之后投递会失败并被记录，
        // 那是"前台程序已经不读了"的真实信号，比静默吃内存好。
        let (sender, receiver) = async_channel::bounded::<Vec<u8>>(PTY_WRITE_QUEUE_CAPACITY);

        let _ = thread::Builder::new()
            .name("agent-pty-writer".to_string())
            .spawn(move || {
                while let Ok(bytes) = receiver.recv_blocking() {
                    if let Err(err) = write_to_pty(&writer, &bytes) {
                        eprintln!("pty write failed: {err:#}");
                        break;
                    }
                }
            });

        Self { sender }
    }

    /// 投递字节。不阻塞 UI 线程。
    ///
    /// 返回 false 表示没投进去（队列满，或写线程已经没了）。调用方通常只需要记录，
    /// 因为这两种情况都意味着这次写本来也到不了子进程。
    pub(crate) fn write(&self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return true;
        }

        match self.sender.try_send(bytes.to_vec()) {
            Ok(()) => true,
            Err(async_channel::TrySendError::Full(_)) => {
                eprintln!(
                    "pty write queue is full ({} bytes dropped); is the foreground program reading stdin?",
                    bytes.len()
                );
                false
            }
            Err(async_channel::TrySendError::Closed(_)) => false,
        }
    }
}

fn child_term() -> String {
    std::env::var("AGENT_TUI_TERM")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CHILD_TERM.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn default_child_term_uses_xterm_standout() {
        assert_eq!(DEFAULT_CHILD_TERM, "xterm-256color");
    }

    /// 一个永远写不动的 writer，模拟"前台程序不读 stdin"。
    struct BlockingWriter {
        release: Arc<Mutex<bool>>,
    }

    impl Write for BlockingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            while !*self.release.lock() {
                thread::sleep(Duration::from_millis(5));
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// 回归（ROB-2）：写入必须立刻返回。此前是在调用方线程上 write_all + flush，
    /// 而调用方是 UI 线程——前台程序不读 stdin 时（Ctrl-S 流控、sleep 中），
    /// 粘贴超过内核 PTY 缓冲就会把整个窗口卡死。
    #[test]
    fn writes_do_not_block_the_caller_when_the_pty_stalls() {
        let release = Arc::new(Mutex::new(false));
        let writer: SharedPtyWriter = Arc::new(Mutex::new(Box::new(BlockingWriter {
            release: release.clone(),
        })));
        let handle = PtyWriteHandle::spawn(writer);

        let started = Instant::now();
        for _ in 0..64 {
            assert!(handle.write(b"blocked payload"));
        }
        let elapsed = started.elapsed();

        // 写线程此刻正卡在第一个 write 里；投递方却应该毫不受影响。
        assert!(
            elapsed < Duration::from_millis(200),
            "enqueueing must not wait on the PTY; took {elapsed:?}"
        );

        *release.lock() = true;
    }

    #[test]
    fn writes_reach_the_pty_in_order() {
        let recorded = Arc::new(Mutex::new(Vec::<u8>::new()));
        let writer: SharedPtyWriter = Arc::new(Mutex::new(Box::new(RecordingWriter {
            recorded: recorded.clone(),
        })));
        let handle = PtyWriteHandle::spawn(writer);

        // 顺序对转义序列是硬要求（也决定 DSR 应答相对普通输入的位置）。
        for chunk in [b"\x1b[".as_slice(), b"1;4", b"R"] {
            assert!(handle.write(chunk));
        }

        for _ in 0..200 {
            if recorded.lock().as_slice() == b"\x1b[1;4R" {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!(
            "bytes arrived out of order or not at all: {:?}",
            recorded.lock()
        );
    }

    #[test]
    fn empty_writes_are_accepted_without_touching_the_pty() {
        let recorded = Arc::new(Mutex::new(Vec::<u8>::new()));
        let writer: SharedPtyWriter = Arc::new(Mutex::new(Box::new(RecordingWriter {
            recorded: recorded.clone(),
        })));
        let handle = PtyWriteHandle::spawn(writer);

        assert!(handle.write(b""));
        thread::sleep(Duration::from_millis(50));
        assert!(recorded.lock().is_empty());
    }

    struct RecordingWriter {
        recorded: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for RecordingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.recorded.lock().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
