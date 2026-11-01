//! Fresh owned native-diagnostic process per model/round, with phase watchdogs.
use super::{Arguments, corpus::Case};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const FRAME_LIMIT: usize = 2 * 1024 * 1024;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(30);
fn exit_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return -signal;
        }
    }
    -1
}
struct Owner {
    child: Child,
    reader: Option<JoinHandle<()>>,
}
impl Owner {
    fn finish(&mut self, timeout: Duration) -> Result<i32> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                if let Some(reader) = self.reader.take() {
                    let _ = reader.join();
                }
                return Ok(exit_code(status));
            }
            ensure!(
                Instant::now() < deadline,
                "Direct native child did not exit before its close deadline"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn terminate(&mut self) -> Option<i32> {
        let _ = self.child.kill();
        let code = self.child.wait().ok().map(exit_code);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        code
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

fn spawn(
    request: &Value,
    profile: &Path,
) -> Result<(Owner, Receiver<std::result::Result<Value, String>>)> {
    let mut child = Command::new(std::env::current_exe()?)
        .arg("--benchmark-direct-child")
        .env("INKLOW_HOME", profile)
        .env("HOME", profile)
        .env("XDG_CONFIG_HOME", profile)
        .env("XDG_DATA_HOME", profile)
        .env("XDG_CACHE_HOME", profile)
        .env("TMPDIR", profile)
        .env("HF_HUB_OFFLINE", "1")
        .env("TRANSFORMERS_OFFLINE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Cannot start owned direct native benchmark process")?;
    let setup = (|| -> Result<_> {
        let mut stdin = child
            .stdin
            .take()
            .context("Direct benchmark stdin unavailable")?;
        serde_json::to_writer(&mut stdin, request)?;
        stdin.write_all(b"\n")?;
        drop(stdin);
        let stdout = child
            .stdout
            .take()
            .context("Direct benchmark stdout unavailable")?;
        // Unbounded channel is byte-bounded by at most the fixed authored suite
        // (one row per case), not arbitrary model tokens; each frame has a cap.
        let (tx, rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut frame = Vec::new();
                let read = reader
                    .by_ref()
                    .take((FRAME_LIMIT + 1) as u64)
                    .read_until(b'\n', &mut frame);
                match read {
                    Ok(0) => break,
                    Ok(_) if frame.len() <= FRAME_LIMIT && frame.last() == Some(&b'\n') => {
                        let value = serde_json::from_slice(&frame)
                            .map_err(|_| "Invalid direct benchmark child JSON".to_owned());
                        let failed = value.is_err();
                        if tx.send(value).is_err() || failed {
                            break;
                        }
                    }
                    _ => {
                        let _ = tx.send(Err(
                            "Invalid or oversized direct benchmark child frame".into()
                        ));
                        break;
                    }
                }
            }
        });
        Ok((reader, rx))
    })();
    match setup {
        Ok((reader, rx)) => Ok((
            Owner {
                child,
                reader: Some(reader),
            },
            rx,
        )),
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
        }
    }
}

pub fn session(
    args: &Arguments,
    root: &Path,
    model: &Path,
    round: usize,
    cases: &[Case],
    tokens: u32,
    words: u32,
) -> Value {
    let mut result = json!({"round":round + 1, "model_path":model, "cases":[], "cancellation":[], "close":{"status":"not_started"}, "measurement_path":"direct_native_provider"});
    let mut watchdog = false;
    let mut failed_phase = None;
    let execution = (|| -> Result<()> {
        let profile = tempfile::Builder::new()
            .prefix("direct-native-")
            .tempdir_in(root)?;
        let request = json!({"model":model, "round_index":round, "max_tokens":tokens, "max_words":words,
            "prompt_style":args.prompt_style, "acceleration":args.acceleration,
            "cases":cases.iter().map(|case| &case.id).collect::<Vec<_>>()});
        let (mut owner, messages) = spawn(&request, profile.path())?;
        let mut deadline = Instant::now() + Duration::from_secs(args.prepare_timeout_seconds);
        let mut phase = "startup".to_owned();
        let mut final_result = false;
        loop {
            match messages.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Ok(message)) => {
                    if let Some(next) = message["phase"].as_str() {
                        phase = next.to_owned();
                        let timeout = match next {
                            "prepare" => args.prepare_timeout_seconds,
                            "close" => 30,
                            _ => args.case_timeout_seconds,
                        };
                        deadline = Instant::now() + Duration::from_secs(timeout);
                    } else if let Some(row) = message.get("case_result") {
                        result["cases"]
                            .as_array_mut()
                            .expect("cases")
                            .push(row.clone());
                    } else if let Some(probe) = message.get("cancellation_result") {
                        result["cancellation"]
                            .as_array_mut()
                            .expect("cancellation")
                            .push(probe.clone());
                    } else if let Some(prepared) = message.get("prepared") {
                        if let Some(fields) = prepared.as_object() {
                            for (key, value) in fields {
                                result[key] = value.clone();
                            }
                        }
                    } else if let Some(complete) = message.get("result") {
                        result = complete.clone();
                        final_result = true;
                        break;
                    } else {
                        anyhow::bail!("Direct child returned an unknown frame");
                    }
                }
                Ok(Err(error)) => anyhow::bail!(error),
                Err(RecvTimeoutError::Timeout) => {
                    watchdog = true;
                    failed_phase = Some(phase);
                    let code = owner.terminate();
                    result["process"] = json!({"exitcode":code, "terminated_by_watchdog":true, "failed_phase":failed_phase, "fresh_owned_process":true});
                    anyhow::bail!("Direct native child exceeded the current phase deadline");
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        let exitcode = match owner.finish(CLOSE_TIMEOUT) {
            Ok(code) => code,
            Err(error) => {
                watchdog = true;
                failed_phase = Some("process_exit".into());
                let code = owner.terminate();
                result["process"] = json!({"exitcode":code, "terminated_by_watchdog":true, "failed_phase":failed_phase, "fresh_owned_process":true});
                return Err(error);
            }
        };
        result["process"] = json!({"exitcode":exitcode, "terminated_by_watchdog":false, "fresh_owned_process":true});
        ensure!(
            final_result && exitcode == 0,
            "Direct native child exited without a successful complete report"
        );
        Ok(())
    })();
    if let Err(error) = execution {
        result["error"] = json!(error.to_string());
    }
    if result["process"].is_null() {
        result["process"] = json!({"exitcode":null, "terminated_by_watchdog":watchdog, "failed_phase":failed_phase, "fresh_owned_process":true});
    }
    result
}

/// Private example-only dispatch, before ordinary CLI/profile validation. This
/// is not a production flag or a Python-compatible runtime shim.
pub fn child_main() -> i32 {
    let result = (|| -> Result<()> {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take((FRAME_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= FRAME_LIMIT,
            "Direct child request is oversized"
        );
        let request: Value = serde_json::from_slice(&bytes)?;
        let stdout = std::io::stdout();
        let mut output = stdout.lock();
        let mut emit = |message: Value| -> Result<()> {
            serde_json::to_writer(&mut output, &message)?;
            output.write_all(b"\n")?;
            output.flush()?;
            Ok(())
        };
        let result = super::direct::run(&request, &mut emit)?;
        emit(json!({"result":result}))?;
        Ok(())
    })();
    if result.is_ok() { 0 } else { 1 }
}
