//! Platform-neutral process handle: reader/writer threads, exit waiting, tree killing.

use std::io::{self, Read, Write};
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use crate::OutputChunk;

/// Kills a whole process tree (Windows: terminates the Job Object; Unix: signals the process
/// group). Dropping the last reference also kills the tree.
pub(crate) trait KillTree: Send + Sync {
    fn kill_tree(&self);
}

/// What a platform launcher hands back.
pub(crate) struct Launched {
    pub pid: u32,
    pub sandboxed: bool,
    pub stdin: Box<dyn Write + Send>,
    pub stdout: Box<dyn Read + Send>,
    pub stderr: Box<dyn Read + Send>,
    /// Blocks until the main process exits; returns its exit code.
    pub waiter: Box<dyn FnOnce() -> Option<i32> + Send>,
    pub killer: Arc<dyn KillTree>,
}

enum StdinMsg {
    Data(Vec<u8>, Option<oneshot::Sender<io::Result<()>>>),
}

/// Cheap handle that can kill the process tree from anywhere.
#[derive(Clone)]
pub(crate) struct TreeKiller(Arc<dyn KillTree>);

impl TreeKiller {
    pub(crate) fn kill_tree(&self) {
        self.0.kill_tree();
    }
}

/// A running child process with piped stdio (no PTY).
///
/// Output from stdout and stderr arrives, in order, on a single channel (see
/// [`SpawnedProcess::take_output`] / [`SpawnedProcess::recv_output`]). Dropping the handle kills
/// the whole process tree.
pub struct SpawnedProcess {
    pid: u32,
    sandboxed: bool,
    stdin_tx: Option<std::sync::mpsc::Sender<StdinMsg>>,
    output_rx: Option<mpsc::Receiver<OutputChunk>>,
    exit_rx: Option<oneshot::Receiver<Option<i32>>>,
    exit: Option<Option<i32>>,
    killer: Arc<dyn KillTree>,
}

impl std::fmt::Debug for SpawnedProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnedProcess")
            .field("pid", &self.pid)
            .field("sandboxed", &self.sandboxed)
            .field("exit", &self.exit)
            .finish_non_exhaustive()
    }
}

const READ_CHUNK: usize = 8 * 1024;
const OUTPUT_CHANNEL_CAPACITY: usize = 256;

impl SpawnedProcess {
    pub(crate) fn start(launched: Launched, initial_stdin: Option<Vec<u8>>) -> SpawnedProcess {
        let Launched { pid, sandboxed, stdin, stdout, stderr, waiter, killer } = launched;
        let (out_tx, out_rx) = mpsc::channel(OUTPUT_CHANNEL_CAPACITY);
        spawn_reader(stdout, out_tx.clone(), false);
        spawn_reader(stderr, out_tx, true);

        let (stdin_tx, stdin_rx) = std::sync::mpsc::channel::<StdinMsg>();
        spawn_writer(stdin, stdin_rx);

        let (exit_tx, exit_rx) = oneshot::channel();
        let _ = std::thread::Builder::new().name("odex-sandbox-wait".into()).spawn(move || {
            let code = waiter();
            let _ = exit_tx.send(code);
        });

        let process = SpawnedProcess {
            pid,
            sandboxed,
            stdin_tx: Some(stdin_tx),
            output_rx: Some(out_rx),
            exit_rx: Some(exit_rx),
            exit: None,
            killer,
        };
        if let Some(data) = initial_stdin {
            process.queue_stdin(data);
        }
        process
    }

    /// OS process id of the main process.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Whether the process runs under a sandbox backend.
    pub fn sandboxed(&self) -> bool {
        self.sandboxed
    }

    /// Writes `data` to the child's stdin, waiting until it has been handed to the pipe.
    pub async fn write_stdin(&self, data: &[u8]) -> io::Result<()> {
        let tx = self.stdin_tx.as_ref().ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "stdin is closed"))?;
        let (ack_tx, ack_rx) = oneshot::channel();
        tx.send(StdinMsg::Data(data.to_vec(), Some(ack_tx)))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "stdin writer has exited"))?;
        ack_rx.await.unwrap_or_else(|_| Err(io::Error::new(io::ErrorKind::BrokenPipe, "stdin writer has exited")))
    }

    /// Queues `data` for stdin without waiting.
    pub(crate) fn queue_stdin(&self, data: Vec<u8>) {
        if let Some(tx) = &self.stdin_tx {
            let _ = tx.send(StdinMsg::Data(data, None));
        }
    }

    /// Closes stdin once all queued data has been written (the child sees EOF).
    pub fn close_stdin(&mut self) {
        self.stdin_tx = None;
    }

    /// Kills the whole process tree. Idempotent.
    pub fn kill(&self) {
        self.killer.kill_tree();
    }

    pub(crate) fn killer(&self) -> TreeKiller {
        TreeKiller(self.killer.clone())
    }

    /// Waits for the main process to exit and returns its exit code. Cancel-safe.
    pub async fn wait(&mut self) -> Option<i32> {
        if let Some(code) = self.exit {
            return code;
        }
        let code = match self.exit_rx.as_mut() {
            Some(rx) => rx.await.unwrap_or(None),
            None => None,
        };
        self.exit_rx = None;
        self.exit = Some(code);
        code
    }

    /// `Some(exit code)` if the main process has exited, `None` while it is running.
    pub fn try_wait(&mut self) -> Option<Option<i32>> {
        if self.exit.is_some() {
            return self.exit;
        }
        let rx = self.exit_rx.as_mut()?;
        match rx.try_recv() {
            Ok(code) => {
                self.exit = Some(code);
                self.exit_rx = None;
                self.exit
            }
            Err(oneshot::error::TryRecvError::Empty) => None,
            Err(oneshot::error::TryRecvError::Closed) => {
                self.exit = Some(None);
                self.exit_rx = None;
                self.exit
            }
        }
    }

    /// Takes the output receiver (stdout and stderr chunks in arrival order). The channel closes
    /// once every process holding the pipes has exited.
    pub fn take_output(&mut self) -> Option<mpsc::Receiver<OutputChunk>> {
        self.output_rx.take()
    }

    /// Receives the next output chunk; `None` when the output is closed (or was taken).
    pub async fn recv_output(&mut self) -> Option<OutputChunk> {
        match self.output_rx.as_mut() {
            Some(rx) => rx.recv().await,
            None => None,
        }
    }
}

impl Drop for SpawnedProcess {
    fn drop(&mut self) {
        self.killer.kill_tree();
    }
}

fn spawn_reader(mut reader: Box<dyn Read + Send>, tx: mpsc::Sender<OutputChunk>, is_stderr: bool) {
    let name = if is_stderr { "odex-sandbox-stderr" } else { "odex-sandbox-stdout" };
    let _ = std::thread::Builder::new().name(name.into()).spawn(move || {
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let data = buf[..n].to_vec();
                    let chunk = if is_stderr { OutputChunk::Stderr(data) } else { OutputChunk::Stdout(data) };
                    if tx.blocking_send(chunk).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
}

fn spawn_writer(mut writer: Box<dyn Write + Send>, rx: std::sync::mpsc::Receiver<StdinMsg>) {
    let _ = std::thread::Builder::new().name("odex-sandbox-stdin".into()).spawn(move || {
        let mut broken = false;
        while let Ok(StdinMsg::Data(data, ack)) = rx.recv() {
            let result = if broken {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "stdin pipe is closed"))
            } else {
                writer.write_all(&data).and_then(|_| writer.flush())
            };
            if result.is_err() {
                broken = true;
            }
            if let Some(ack) = ack {
                let _ = ack.send(result);
            }
        }
        // Dropping the writer closes the pipe: the child sees EOF.
        drop(writer);
    });
}
