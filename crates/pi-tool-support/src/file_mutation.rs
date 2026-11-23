//! Shared file-mutation ordering for Pi's write and edit tools.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use pi_plugin::ToolError;
use tokio::sync::oneshot;

#[derive(Default)]
struct Queues {
    next_id: u64,
    tails: HashMap<PathBuf, (u64, oneshot::Receiver<()>)>,
}

fn queues() -> &'static Mutex<Queues> {
    static QUEUES: OnceLock<Mutex<Queues>> = OnceLock::new();
    QUEUES.get_or_init(Default::default)
}

struct Release {
    path: PathBuf,
    id: u64,
    _next: oneshot::Sender<()>,
}

impl Drop for Release {
    fn drop(&mut self) {
        let mut queues = queues()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if queues
            .tails
            .get(&self.path)
            .is_some_and(|(id, _)| *id == self.id)
        {
            queues.tails.remove(&self.path);
        }
        // Dropping the sender releases the successor even after an error/panic.
    }
}

/// Resolve aliases and register mutations in invocation order, while unrelated
/// files remain independent. The owned operation keeps its place and lock until
/// all in-flight filesystem work settles, even if its caller stops awaiting it.
/// Operations should observe their abort signal between (not during) I/O awaits.
pub async fn with_file_mutation_queue<T: Send + 'static>(
    path: PathBuf,
    operation: impl Future<Output = Result<T, ToolError>> + Send + 'static,
) -> Result<T, ToolError> {
    let (previous, release) = {
        let mut queues = queues()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = crate::absolute_lexical_path(&path)?;
        let path = match std::fs::canonicalize(&path) {
            Ok(path) => path,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                path
            }
            Err(error) => return Err(crate::execution(error.to_string())),
        };
        queues.next_id = queues.next_id.wrapping_add(1);
        let id = queues.next_id;
        let (next, tail) = oneshot::channel();
        let previous = queues
            .tails
            .insert(path.clone(), (id, tail))
            .map(|(_, tail)| tail);
        (
            previous,
            Release {
                path,
                id,
                _next: next,
            },
        )
    };
    tokio::spawn(async move {
        let _release = release;
        if let Some(previous) = previous {
            let _ = previous.await;
        }
        operation.await
    })
    .await
    .map_err(|error| crate::execution(format!("file mutation task failed: {error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test(flavor = "current_thread")]
    async fn same_file_operations_run_in_registration_order() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        let order = Arc::new(Mutex::new(Vec::new()));
        let (finish, held) = oneshot::channel();

        let first_order = Arc::clone(&order);
        let first = tokio::spawn(with_file_mutation_queue(path.clone(), async move {
            first_order.lock().unwrap().push("first:start");
            let _ = held.await;
            first_order.lock().unwrap().push("first:end");
            Ok(())
        }));
        tokio::task::yield_now().await;

        let second_order = Arc::clone(&order);
        let second = tokio::spawn(with_file_mutation_queue(path, async move {
            second_order.lock().unwrap().push("second:start");
            second_order.lock().unwrap().push("second:end");
            Ok(())
        }));
        tokio::task::yield_now().await;
        assert_eq!(*order.lock().unwrap(), ["first:start"]);

        finish.send(()).unwrap();
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        assert_eq!(
            *order.lock().unwrap(),
            ["first:start", "first:end", "second:start", "second:end"]
        );
    }

    #[tokio::test]
    async fn same_file_waits_through_caller_cancellation_and_other_files_do_not() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        let (entered, ready) = oneshot::channel();
        let (finish, held) = oneshot::channel();
        let first = tokio::spawn(with_file_mutation_queue(path.clone(), async move {
            let _ = entered.send(());
            let _ = held.await;
            Err::<(), _>(crate::execution("first failed"))
        }));
        ready.await.unwrap();
        first.abort();
        let second = tokio::spawn(with_file_mutation_queue(path, async { Ok(2) }));
        let unrelated = with_file_mutation_queue(root.path().join("other"), async { Ok(3) })
            .await
            .unwrap();
        assert_eq!(unrelated, 3);
        tokio::task::yield_now().await;
        assert!(!second.is_finished());
        finish.send(()).unwrap();
        assert_eq!(second.await.unwrap().unwrap(), 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_aliases_share_the_mutation_queue() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file");
        std::fs::write(&path, "before").unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        let (entered, ready) = oneshot::channel();
        let (finish, held) = oneshot::channel();
        let first = tokio::spawn(with_file_mutation_queue(path, async move {
            let _ = entered.send(());
            let _ = held.await;
            Ok(())
        }));
        ready.await.unwrap();
        let second = tokio::spawn(with_file_mutation_queue(alias, async { Ok(2) }));
        tokio::task::yield_now().await;
        assert!(!second.is_finished());
        finish.send(()).unwrap();
        first.await.unwrap().unwrap();
        assert_eq!(second.await.unwrap().unwrap(), 2);
    }
}
