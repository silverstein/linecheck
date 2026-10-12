//! Bounded execution for the post-session speech helper.

use std::io::{self, Read};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Drain both pipes while the child runs (large transcripts must not deadlock
/// on a full pipe), and reap the process even when the recognizer hangs.
pub fn output_with_timeout(command: &mut Command, timeout: Duration) -> io::Result<Output> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let (sender, receiver) = std::sync::mpsc::channel();
    for (index, mut pipe) in [
        (0, Box::new(stdout) as Box<dyn Read + Send>),
        (1, Box::new(stderr) as Box<dyn Read + Send>),
    ] {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            // The transcript is bounded separately before alignment. Bound
            // helper output too, including malformed/noisy diagnostic output.
            const MAX_OUTPUT: u64 = 16 * 1024 * 1024;
            let result = pipe
                .by_ref()
                .take(MAX_OUTPUT + 1)
                .read_to_end(&mut bytes)
                .and_then(|_| {
                    if bytes.len() as u64 > MAX_OUTPUT {
                        Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "speech helper output is too large",
                        ))
                    } else {
                        Ok(bytes)
                    }
                });
            let _ = sender.send((index, result));
        });
    }
    drop(sender);
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(25));
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(result.err().unwrap_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "speech verification timed out")
                }));
            }
        }
    };
    let status = status?;
    let mut streams = [Vec::new(), Vec::new()];
    for _ in 0..2 {
        // A descendant could retain a pipe even after its parent exits. Do
        // not block the caller on reader-thread joins beyond the deadline.
        let remaining = timeout.saturating_sub(started.elapsed());
        let (index, result) = receiver.recv_timeout(remaining).map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "speech helper output did not close in time",
            )
        })?;
        streams[index] = result?;
    }
    let [stdout, stderr] = streams;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn drains_both_pipes_and_preserves_exit_status() {
        let output = output_with_timeout(
            Command::new("sh").args(["-c", "printf transcript; printf diagnostic >&2; exit 7"]),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(output.stdout, b"transcript");
        assert_eq!(output.stderr, b"diagnostic");
        assert_eq!(output.status.code(), Some(7));
    }

    #[test]
    fn drains_output_larger_than_a_pipe_buffer() {
        let output = output_with_timeout(
            Command::new("sh").args([
                "-c",
                "head -c 131072 /dev/zero; head -c 131072 /dev/zero >&2",
            ]),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 131072);
        assert_eq!(output.stderr.len(), 131072);
    }

    #[test]
    fn inherited_pipe_cannot_hold_the_caller_past_the_deadline() {
        let started = Instant::now();
        let error = output_with_timeout(
            Command::new("sh").args(["-c", "sleep 1 & exit 0"]),
            Duration::from_millis(50),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_millis(750));
    }

    #[test]
    fn kills_and_reaps_a_stuck_helper() {
        let started = Instant::now();
        let error = output_with_timeout(
            Command::new("sh").args(["-c", "exec sleep 30"]),
            Duration::from_millis(50),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
