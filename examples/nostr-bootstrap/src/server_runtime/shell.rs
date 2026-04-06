use std::env;
use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::*;
use crate::common::now_ms;
use crate::{encode_session_frame, SessionFrame};

impl ServerRuntimeCore {
    pub(crate) async fn ensure_session(
        &self,
        session_id: &str,
        remote: SocketAddr,
    ) -> Arc<Mutex<SessionState>> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get(session_id) {
            return session.clone();
        }
        let session = Arc::new(Mutex::new(SessionState {
            remote,
            cwd: env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            running: None,
        }));
        sessions.insert(session_id.to_owned(), session.clone());
        println!(
            "[session] {} {{\"host\":\"{}\",\"port\":{}}}",
            session_id,
            remote.ip(),
            remote.port()
        );
        session
    }

    async fn send_shell_result(
        &self,
        session_id: &str,
        remote: SocketAddr,
        payload: Value,
    ) -> Result<()> {
        let frame = SessionFrame {
            session_id: session_id.to_owned(),
            frame_type: "response".to_owned(),
            channel: Some("shell_result".to_owned()),
            payload,
            at: now_ms(),
        };
        let bytes = encode_session_frame(&frame)?;
        self.udp_socket.send_to(&bytes, remote).await?;
        Ok(())
    }

    pub(crate) async fn handle_shell_command(
        self: Arc<Self>,
        session_id: String,
        session: Arc<Mutex<SessionState>>,
        command_id: Option<String>,
        command: String,
    ) -> Result<()> {
        let mut guard = session.lock().await;
        let remote = guard.remote;
        if command.is_empty() {
            let cwd = guard.cwd.display().to_string();
            drop(guard);
            self.send_shell_result(
                &session_id,
                remote,
                json!({"id": command_id, "command": command, "ok": true, "code": 0, "stdout": "", "stderr": "", "cwd": cwd, "ts": now_ms()}),
            )
            .await?;
            return Ok(());
        }

        if command == "cd" || command.starts_with("cd ") {
            let target = if command == "cd" {
                env::var("HOME").unwrap_or_else(|_| guard.cwd.display().to_string())
            } else {
                command[3..].trim().to_owned()
            };
            let resolved = if PathBuf::from(&target).is_absolute() {
                PathBuf::from(&target)
            } else {
                guard.cwd.join(&target)
            };
            if resolved.is_dir() {
                guard.cwd = resolved;
                let cwd = guard.cwd.display().to_string();
                drop(guard);
                self.send_shell_result(
                    &session_id,
                    remote,
                    json!({"id": command_id, "command": command, "ok": true, "code": 0, "stdout": "", "stderr": "", "cwd": cwd, "ts": now_ms()}),
                )
                .await?;
            } else {
                let cwd = guard.cwd.display().to_string();
                drop(guard);
                self.send_shell_result(
                    &session_id,
                    remote,
                    json!({"id": command_id, "command": command, "ok": false, "code": 1, "stdout": "", "stderr": format!("cd: no such directory: {target}"), "cwd": cwd, "ts": now_ms()}),
                )
                .await?;
            }
            return Ok(());
        }

        if guard.running.is_some() {
            let cwd = guard.cwd.display().to_string();
            drop(guard);
            self.send_shell_result(
                &session_id,
                remote,
                json!({"id": command_id, "command": command, "ok": false, "code": 1, "stdout": "", "stderr": "another command is still running; press Ctrl-C first", "cwd": cwd, "ts": now_ms()}),
            )
            .await?;
            return Ok(());
        }

        let cwd = guard.cwd.clone();
        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        guard.running = Some(cancel_tx);
        drop(guard);

        let state = self.clone();
        tokio::spawn(async move {
            let mut child = match Command::new("sh")
                .arg("-lc")
                .arg(&command)
                .current_dir(&cwd)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
            {
                Ok(child) => child,
                Err(err) => {
                    let _ = state
                        .send_shell_result(
                            &session_id,
                            remote,
                            json!({"id": command_id, "command": command, "ok": false, "code": 1, "stdout": "", "stderr": err.to_string(), "cwd": cwd.display().to_string(), "ts": now_ms()}),
                        )
                        .await;
                    if let Some(session) = state.sessions.lock().await.get(&session_id).cloned() {
                        session.lock().await.running = None;
                    }
                    return;
                }
            };

            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            let stdout_task = tokio::spawn(async move {
                let mut buf = Vec::new();
                if let Some(mut stdout) = stdout {
                    let _ = stdout.read_to_end(&mut buf).await;
                }
                buf
            });
            let stderr_task = tokio::spawn(async move {
                let mut buf = Vec::new();
                if let Some(mut stderr) = stderr {
                    let _ = stderr.read_to_end(&mut buf).await;
                }
                buf
            });

            let (interrupted, exit_code) = tokio::select! {
                _ = &mut cancel_rx => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    (true, 130)
                }
                status = child.wait() => {
                    match status {
                        Ok(status) => (false, status.code().unwrap_or(1)),
                        Err(err) => {
                            let _ = state
                                .send_shell_result(
                                    &session_id,
                                    remote,
                                    json!({"id": command_id, "command": command, "ok": false, "code": 1, "stdout": "", "stderr": err.to_string(), "cwd": cwd.display().to_string(), "ts": now_ms()}),
                                )
                                .await;
                            if let Some(session) = state.sessions.lock().await.get(&session_id).cloned() {
                                session.lock().await.running = None;
                            }
                            return;
                        }
                    }
                }
            };

            let stdout =
                String::from_utf8_lossy(&stdout_task.await.unwrap_or_default()).to_string();
            let stderr =
                String::from_utf8_lossy(&stderr_task.await.unwrap_or_default()).to_string();
            if let Some(session) = state.sessions.lock().await.get(&session_id).cloned() {
                session.lock().await.running = None;
            }

            let payload = if interrupted {
                json!({"id": command_id, "command": command, "ok": false, "code": exit_code, "stdout": stdout, "stderr": if stderr.is_empty() { "Interrupted (SIGINT)" } else { stderr.as_str() }, "cwd": cwd.display().to_string(), "ts": now_ms()})
            } else {
                json!({"id": command_id, "command": command, "ok": exit_code == 0, "code": exit_code, "stdout": stdout, "stderr": stderr, "cwd": cwd.display().to_string(), "ts": now_ms()})
            };

            let _ = state.send_shell_result(&session_id, remote, payload).await;
        });

        Ok(())
    }

    pub(crate) async fn handle_shell_interrupt(
        &self,
        _session_id: &str,
        session: Arc<Mutex<SessionState>>,
    ) -> Result<()> {
        let mut guard = session.lock().await;
        if let Some(cancel) = guard.running.take() {
            let _ = cancel.send(());
        }
        drop(guard);
        Ok(())
    }
}
