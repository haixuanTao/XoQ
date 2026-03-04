#![allow(clippy::useless_conversion)]

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::time::Duration;

/// Run a closure in a dedicated thread to avoid "Cannot start a runtime from
/// within a runtime" errors when called from an existing async context.
fn run_in_thread<F, T, E>(f: F) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: Send + 'static,
{
    std::thread::spawn(f).join().expect("Thread panicked")
}

/// A chat message received from another user.
///
/// Attributes:
///     name (str): Sender's username
///     text (str): Message text
///     ts (int): Timestamp in milliseconds since epoch
#[pyclass]
#[derive(Clone)]
pub struct ChatMessage {
    #[pyo3(get)]
    name: String,
    #[pyo3(get)]
    text: String,
    #[pyo3(get)]
    ts: u64,
}

#[pymethods]
impl ChatMessage {
    fn __repr__(&self) -> String {
        format!(
            "ChatMessage(name={:?}, text={:?}, ts={})",
            self.name, self.text, self.ts
        )
    }

    fn __str__(&self) -> String {
        format!("{}: {}", self.name, self.text)
    }
}

/// MoQ chat client — connect to a relay and send/receive chat messages.
///
/// Interoperable with the browser chat in openarm.html.
///
/// Example:
///     chat = Chat('anon/openarm-chat', username='robot')
///     chat.send("hello world")
///     msg = chat.recv(timeout=5.0)
///     print(f"{msg.name}: {msg.text}")
#[pyclass]
pub struct Chat {
    client: xoq::chat::ChatClient,
}

#[pymethods]
impl Chat {
    /// Create a new chat client and connect to the relay.
    ///
    /// Args:
    ///     channel: Chat channel path (e.g., 'anon/openarm-chat')
    ///     relay: MoQ relay URL (default: 'https://cdn.1ms.ai')
    ///     username: Display name for outgoing messages (default: 'python')
    #[new]
    #[pyo3(signature = (channel, relay="https://cdn.1ms.ai", username="python"))]
    fn new(channel: &str, relay: &str, username: &str) -> PyResult<Self> {
        let channel = channel.to_string();
        let relay = relay.to_string();
        let username = username.to_string();

        let client = run_in_thread(move || {
            xoq::chat::ChatClient::connect(&relay, &channel, &username)
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))
        })?;

        Ok(Chat { client })
    }

    /// Send a chat message.
    ///
    /// Args:
    ///     text: Message text to send
    fn send(&self, text: &str) -> PyResult<()> {
        self.client
            .send(text)
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))
    }

    /// Receive the next chat message.
    ///
    /// Args:
    ///     timeout: Timeout in seconds (None for blocking)
    ///
    /// Returns:
    ///     ChatMessage or None if timeout expires
    #[pyo3(signature = (timeout=None))]
    fn recv(&self, py: Python<'_>, timeout: Option<f64>) -> PyResult<Option<ChatMessage>> {
        let dur = timeout.map(Duration::from_secs_f64);
        let result = py.allow_threads(|| {
            self.client
                .recv(dur)
                .map_err(|e| PyRuntimeError::new_err(e.to_string()))
        })?;

        Ok(result.map(|m| ChatMessage {
            name: m.name,
            text: m.text,
            ts: m.ts,
        }))
    }

    /// Get the username.
    #[getter]
    fn username(&self) -> &str {
        self.client.username()
    }

    /// Context manager enter.
    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Context manager exit.
    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &self,
        _exc_type: Option<&pyo3::Bound<'_, pyo3::types::PyAny>>,
        _exc_val: Option<&pyo3::Bound<'_, pyo3::types::PyAny>>,
        _exc_tb: Option<&pyo3::Bound<'_, pyo3::types::PyAny>>,
    ) -> PyResult<bool> {
        Ok(false)
    }

    /// Iterator protocol — returns self.
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Iterator protocol — get next message (1s timeout per iteration).
    fn __next__(&self, py: Python<'_>) -> PyResult<Option<ChatMessage>> {
        self.recv(py, Some(1.0))
    }
}

#[pymodule]
fn xoq_chat(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<ChatMessage>()?;
    m.add_class::<Chat>()?;
    Ok(())
}
