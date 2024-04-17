//
// Copyright (c) 2024 Nathan Fiedler
//
use rusqlite::Connection;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;

/// Task runner job.
type Job = Box<dyn FnOnce() + Send + 'static>;

///
/// This type represents all possible errors that can occur within this crate.
///
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// Error occurred during an I/O related operation.
    #[error("I/O error: {0}")]
    IOError(#[from] std::io::Error),
    /// Error occurred during an SQL related operation.
    #[error("SQL error: {0}")]
    SQLError(#[from] rusqlite::Error),
    /// Error occurred during an mpsc related operation.
    #[error("async mpsc error: {0}")]
    JobError(#[from] std::sync::mpsc::SendError<Job>),
    /// When writing file content to a blob, the result was incomplete.
    #[error("could not write entire file part to blob")]
    IncompleteBlobWrite,
    /// The named pack file was not one of ours.
    #[error("pack file format not recognized")]
    NotPackFile,
    /// The symbolic link bytes were not decipherable.
    #[error("symbolic link encoding was not recognized")]
    LinkTextEncoding,
    /// Something happened when operating on the database.
    #[error("error resulting from database operation")]
    Database,
    /// Thread pool is shutting down
    #[error("thread pool is shutting down")]
    ThreadPoolShutdown,
}

///
/// Use `new()` to create a thread pool and `execute()` to send functions to be
/// executed on the worker threads. To wait for all pending tasks to finish,
/// call `wait_until_done()`, or simply drop the pool.
///
pub struct TaskRunner {
    // pool of workers to facilitate a clean shutdown
    workers: Vec<Worker>,
    // channel through which tasks are sent to workers
    sender: Option<mpsc::SyncSender<crate::Job>>,
    // job_tracker 2-tuple that tracks jobs-started and jobs-completed
    job_tracker: Arc<(Mutex<(u64, u64)>, Condvar)>,
}

impl TaskRunner {
    ///
    /// Create a new TaskRunner that will run jobs on separate threads.
    ///
    /// The `size` is the number of threads in the pool. If zero, will try to
    /// get the number of CPU cores available, defaulting to 1 if error.
    ///
    pub fn new(size: usize) -> TaskRunner {
        let cpu_count = std::thread::available_parallelism().map_or(1, |e| e.get());
        let pool_size = if size == 0 { cpu_count } else { size };
        let (sender, receiver) = mpsc::sync_channel(pool_size);
        let receiver = Arc::new(Mutex::new(receiver));
        let tracker: Arc<(Mutex<(u64, u64)>, Condvar)> =
            Arc::new((Mutex::new((0, 0)), Condvar::new()));
        let mut workers = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            workers.push(Worker::new(Arc::clone(&receiver), tracker.clone()));
        }
        TaskRunner {
            workers,
            sender: Some(sender),
            job_tracker: tracker,
        }
    }

    ///
    /// Execute the given function on a worker in the pool.
    ///
    /// The call will block if the pool is busy.
    ///
    pub fn execute<F>(&mut self, f: F) -> Result<(), crate::Error>
    where
        F: FnOnce() + Send + 'static,
    {
        if let Some(sender) = self.sender.as_ref() {
            let job = Box::new(f);
            // bump the jobs-started counter by one
            let (lock, cvar) = &*self.job_tracker;
            let mut counts = lock.lock().unwrap();
            counts.0 += 1;
            cvar.notify_all();
            Ok(sender.send(job)?)
        } else {
            Err(crate::Error::ThreadPoolShutdown)
        }
    }

    ///
    /// Wait for the number of completed jobs to equal the number of started jobs.
    ///
    /// Returns the number of completed jobs.
    ///
    pub fn wait_until_done(&mut self) -> u64 {
        let (lock, cvar) = &*self.job_tracker;
        let mut counts = lock.lock().unwrap();
        while counts.0 != counts.1 {
            counts = cvar.wait(counts).unwrap();
        }
        counts.1
    }

    ///
    /// Returns the size of the thread pool.
    ///
    pub fn size(&self) -> usize {
        self.workers.len()
    }
}

impl Drop for TaskRunner {
    fn drop(&mut self) {
        drop(self.sender.take());
        for worker in &mut self.workers {
            if let Some(thread) = worker.thread.take() {
                thread.join().expect("failed to join thread")
            }
        }
    }
}

struct Worker {
    thread: Option<thread::JoinHandle<()>>,
}

impl Worker {
    fn new(
        receiver: Arc<Mutex<mpsc::Receiver<crate::Job>>>,
        tracker: Arc<(Mutex<(u64, u64)>, Condvar)>,
    ) -> Worker {
        let thread = thread::spawn(move || loop {
            let message = match receiver.lock() {
                Ok(guard) => guard.recv(),
                Err(poisoned) => {
                    // hard to imagine how this would matter
                    poisoned.into_inner().recv()
                }
            };
            match message {
                Ok(job) => {
                    job();
                }
                Err(_) => {
                    break;
                }
            }
            // bump the jobs-completed counter by one
            let (lock, cvar) = &*tracker;
            let mut counts = lock.lock().unwrap();
            counts.1 += 1;
            cvar.notify_all();
        });
        Worker {
            thread: Some(thread),
        }
    }
}

// Expected SQLite database header: "SQLite format 3\0"
static SQL_HEADER: &'static [u8] = &[
    0x53, 0x51, 0x4c, 0x69, 0x74, 0x65, 0x20, 0x66, 0x6f, 0x72, 0x6d, 0x61, 0x74, 0x20, 0x33, 0x00,
];

///
/// Return `true` if the path refers to a pack file, false otherwise.
///
pub fn is_pack_file<P: AsRef<Path>>(path: P) -> Result<bool, Error> {
    let metadata = fs::metadata(path.as_ref())?;
    if metadata.is_file() && metadata.len() > 16 {
        let mut file = fs::File::open(path.as_ref())?;
        let mut buffer = [0; 16];
        file.read_exact(&mut buffer)?;
        if buffer == SQL_HEADER {
            // open and check for non-zero amount of data
            let conn = Connection::open(path.as_ref())?;
            match conn.prepare("SELECT * FROM item") {
                Ok(mut stmt) => {
                    let result = stmt.exists([])?;
                    return Ok(result);
                }
                Err(_) => {
                    return Ok(false);
                }
            };
        }
    }
    Ok(false)
}

///
/// Return a sanitized version of the path, with any non-normal components
/// removed. Roots and prefixes are especially problematic for extracting an
/// archive, so those are always removed. Note also that path components which
/// refer to the parent directory will be stripped ("foo/../bar" will become
/// "foo/bar").
///
pub fn sanitize_path<P: AsRef<Path>>(dirty: P) -> Result<PathBuf, Error> {
    let components = dirty.as_ref().components();
    let allowed = components.filter(|c| matches!(c, Component::Normal(_)));
    let mut path = PathBuf::new();
    for component in allowed {
        path = path.join(component);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_pack_file() -> Result<(), Error> {
        assert!(!is_pack_file("test/fixtures/empty-file")?);
        assert!(!is_pack_file("test/fixtures/notpack.db3")?);
        assert!(is_pack_file("test/fixtures/pack.db3")?);
        Ok(())
    }

    #[test]
    fn test_sanitize_path() -> Result<(), Error> {
        // need to use real paths for the canonicalize() call
        #[cfg(target_family = "windows")]
        {
            let result = sanitize_path(Path::new("C:\\Windows"))?;
            assert_eq!(result, PathBuf::from("Windows"));
        }
        #[cfg(target_family = "unix")]
        {
            let result = sanitize_path(Path::new("/etc"))?;
            assert_eq!(result, PathBuf::from("etc"));
        }
        let result = sanitize_path(Path::new("src/lib.rs"))?;
        assert_eq!(result, PathBuf::from("src/lib.rs"));

        let result = sanitize_path(Path::new("/usr/../src/./lib.rs"))?;
        assert_eq!(result, PathBuf::from("usr/src/lib.rs"));
        Ok(())
    }

    #[test]
    fn test_task_runner() {
        let counter = Arc::new(Mutex::new(0));
        // scope the pool so it will be dropped and shut down
        {
            let mut pool = TaskRunner::new(4);
            for _ in 0..8 {
                let counter = counter.clone();
                pool.execute(move || {
                    let mut v = counter.lock().unwrap();
                    *v += 1;
                })
                .unwrap();
            }
            let finished = pool.wait_until_done();
            assert_eq!(finished, 8);
        }
        // by now the thread pool has shut down
        let value = counter.lock().unwrap();
        assert_eq!(*value, 8);
    }
}
