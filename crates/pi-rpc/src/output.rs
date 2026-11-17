//! One ordered writer for synchronous extension notifications and awaited RPC output.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
#[cfg(test)]
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

type Completion = oneshot::Sender<Result<(), String>>;

struct Write {
    bytes: Vec<u8>,
    completion: Option<Completion>,
}

struct Output {
    sender: mpsc::UnboundedSender<Write>,
    worker: tokio::task::JoinHandle<()>,
}

impl Drop for Output {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

#[derive(Clone)]
pub(crate) struct RpcOutput(Arc<Output>);

enum Destination {
    Stdout,
    #[cfg(test)]
    Writer(Box<dyn AsyncWrite + Unpin + Send>),
}

#[derive(Default)]
struct WriteCancellation(Arc<AtomicBool>);

impl Drop for WriteCancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl Destination {
    async fn write(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        match self {
            Self::Stdout => {
                let cancellation = WriteCancellation::default();
                let cancelled = cancellation.0.clone();
                tokio::task::spawn_blocking(move || write_stdout(&bytes, &cancelled))
                    .await
                    .map_err(|error| format!("RPC output task failed: {error}"))?
                    .map_err(|error| error.to_string())
            }
            #[cfg(test)]
            Self::Writer(writer) => async {
                writer.write_all(&bytes).await?;
                writer.flush().await
            }
            .await
            .map_err(|error: std::io::Error| error.to_string()),
        }
    }
}

#[cfg(unix)]
fn write_stdout(bytes: &[u8], cancelled: &AtomicBool) -> std::io::Result<()> {
    write_fd(&std::io::stdout(), bytes, cancelled)
}

#[cfg(not(unix))]
fn write_stdout(bytes: &[u8], _cancelled: &AtomicBool) -> std::io::Result<()> {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(bytes)?;
    stdout.flush()
}

#[cfg(unix)]
fn write_fd(
    fd: &impl std::os::fd::AsFd,
    mut bytes: &[u8],
    cancelled: &AtomicBool,
) -> std::io::Result<()> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::io::Errno;

    // Node may make stdout nonblocking. Tokio's blocking stdout adapter uses
    // write_all internally, which loses the partial-write offset on EAGAIN.
    // Write directly and retain that offset while waiting for pipe capacity.
    // This runs on the blocking pool and leaves the host's descriptor flags alone.
    while !bytes.is_empty() {
        if cancelled.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "RPC output cancelled",
            ));
        }
        match rustix::io::write(fd, bytes) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(written) => bytes = &bytes[written..],
            Err(Errno::INTR) => continue,
            Err(Errno::AGAIN) => {
                let mut ready = [PollFd::new(fd, PollFlags::OUT)];
                // A started blocking task cannot be aborted by Tokio. Recheck
                // cancellation even when the client keeps a full pipe open.
                let timeout = Timespec {
                    tv_sec: 0,
                    tv_nsec: 100_000_000,
                };
                match poll(&mut ready, Some(&timeout)) {
                    Ok(_) | Err(Errno::INTR) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

impl RpcOutput {
    pub(crate) fn new() -> Self {
        Self::with_destination(Destination::Stdout)
    }

    #[cfg(test)]
    pub(crate) fn with_writer(writer: impl AsyncWrite + Unpin + Send + 'static) -> Self {
        Self::with_destination(Destination::Writer(Box::new(writer)))
    }

    fn with_destination(mut destination: Destination) -> Self {
        let (sender, mut receiver) = mpsc::unbounded_channel::<Write>();
        let worker = tokio::spawn(async move {
            while let Some(write) = receiver.recv().await {
                let result = destination.write(write.bytes).await;
                let failed = result.is_err();
                if let Some(completion) = write.completion {
                    let _ = completion.send(result);
                }
                if failed {
                    break;
                }
            }
        });
        Self(Arc::new(Output { sender, worker }))
    }

    fn send(&self, value: Option<Value>, completion: Option<Completion>) -> Result<(), String> {
        let bytes = match value {
            Some(value) => {
                let mut bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
                bytes.push(b'\n');
                bytes
            }
            None => Vec::new(),
        };
        self.0
            .sender
            .send(Write { bytes, completion })
            .map_err(|_| "RPC output closed".to_string())
    }

    /// Enqueues at the call site, before a following dialog or command response.
    pub(crate) fn enqueue(&self, value: Value) -> Result<(), String> {
        self.send(Some(value), None)
    }

    pub(crate) async fn emit(&self, value: Value) -> Result<(), String> {
        self.write(Some(value)).await
    }

    /// A barrier that drains earlier notifications as well as awaited writes.
    pub(crate) async fn flush(&self) -> Result<(), String> {
        self.write(None).await
    }

    async fn write(&self, value: Option<Value>) -> Result<(), String> {
        let (completion, result) = oneshot::channel();
        self.send(value, Some(completion))?;
        result.await.map_err(|_| "RPC output closed".to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[cfg(unix)]
    #[test]
    fn nonblocking_output_preserves_bytes_after_pipe_backpressure() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        let padding = [b' '; 8192];
        let mut padding_len = 0;
        loop {
            match writer.write(&padding) {
                Ok(count) => padding_len += count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("filling output buffer: {error}"),
            }
        }
        let expected = serde_json::to_vec(&json!({"catalog":"skill".repeat(512 * 1024)})).unwrap();
        let bytes = expected.clone();
        let writing =
            std::thread::spawn(move || write_fd(&writer, &bytes, &AtomicBool::new(false)));
        let mut received = Vec::new();
        reader.read_to_end(&mut received).unwrap();
        writing.join().unwrap().unwrap();
        assert_eq!(&received[..padding_len], vec![b' '; padding_len]);
        assert_eq!(&received[padding_len..], expected);
    }

    #[cfg(unix)]
    #[test]
    fn disconnected_descriptor_returns_an_error() {
        let (writer, reader) = std::os::unix::net::UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        drop(reader);
        assert_eq!(
            write_fd(&writer, b"closed", &AtomicBool::new(false))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::BrokenPipe,
        );
    }

    #[cfg(unix)]
    #[test]
    fn cancelling_backpressured_output_releases_the_blocking_task() {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        use std::time::Duration;

        let (mut writer, _reader) = UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        while writer.write(&[b' '; 8192]).is_ok() {}
        let cancellation = WriteCancellation::default();
        let cancelled = cancellation.0.clone();
        let (finished, result) = std::sync::mpsc::channel();
        let writing = std::thread::spawn(move || {
            finished
                .send(write_fd(&writer, b"pending", &cancelled))
                .unwrap();
        });
        assert!(result.recv_timeout(Duration::from_millis(20)).is_err());
        drop(cancellation);
        assert_eq!(
            result
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::Interrupted,
        );
        writing.join().unwrap();
    }

    #[tokio::test]
    async fn notifications_precede_responses_and_flush_drains_the_tail() {
        let (writer, reader) = tokio::io::duplex(4096);
        let output = RpcOutput::with_writer(writer);
        output.enqueue(json!({"message":"startup"})).unwrap();
        output.emit(json!({"message":"question"})).await.unwrap();
        output.enqueue(json!({"message":"shutdown"})).unwrap();
        output.flush().await.unwrap();
        drop(output);
        let mut lines = BufReader::new(reader).lines();
        for expected in ["startup", "question", "shutdown"] {
            let record: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(record["message"], expected);
        }
        assert!(lines.next_line().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn disconnected_output_releases_waiting_writes() {
        let (writer, reader) = tokio::io::duplex(16);
        drop(reader);
        let output = RpcOutput::with_writer(writer);
        assert!(output.emit(json!({"message":"closed"})).await.is_err());
        assert!(output.flush().await.is_err());
        assert!(output.enqueue(json!({"message":"closed"})).is_err());
    }
}
