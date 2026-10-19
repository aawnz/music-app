//! The connection to a Jam's relay: a WebSocket on a thread of its own that
//! passes JSON messages both ways. What the messages mean is up to the app
//! (`crate::app::jam`); this only carries them.

use std::sync::mpsc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

/// The relay's address, set when the app is built (`wss://…`).
pub const RELAY: Option<&str> = option_env!("SPOTIFAST_JAM_RELAY");

/// How often the connection proves it is alive, and measures the clock.
const PING_EVERY: Duration = Duration::from_secs(15);

pub enum Incoming {
    Message(Value),
    /// The connection ended, with why.
    Closed(String),
}

pub struct Connection {
    outgoing: tokio::sync::mpsc::UnboundedSender<String>,
    incoming: mpsc::Receiver<Incoming>,
}

impl Connection {
    /// Connects to the Jam `code` at the relay. Messages arrive through
    /// `try_recv`; `ctx` is asked to repaint when one does.
    pub fn open(relay: &str, code: &str, ctx: egui::Context) -> Self {
        let url = format!("{}/jam/{code}", relay.trim_end_matches('/'));
        let (outgoing, mut to_send) = tokio::sync::mpsc::unbounded_channel::<String>();
        let (deliver, incoming) = mpsc::channel();
        std::thread::Builder::new()
            .name("jam".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = deliver.send(Incoming::Closed(error.to_string()));
                        ctx.request_repaint();
                        return;
                    }
                };
                let reason = runtime.block_on(async {
                    let (socket, _) = match tokio::time::timeout(
                        Duration::from_secs(15),
                        tokio_tungstenite::connect_async(url.as_str()),
                    )
                    .await
                    {
                        Ok(Ok(connected)) => connected,
                        Ok(Err(error)) => return format!("Couldn't reach the Jam service ({error})."),
                        Err(_) => return "The Jam service took too long to answer.".to_string(),
                    };
                    let (mut sink, mut stream) = socket.split();
                    let mut ping = tokio::time::interval(PING_EVERY);
                    loop {
                        tokio::select! {
                            next = to_send.recv() => match next {
                                Some(text) => {
                                    if sink.send(Message::Text(text.into())).await.is_err() {
                                        return "The Jam connection dropped.".to_string();
                                    }
                                }
                                // The app let go of the Jam: leave it.
                                None => {
                                    let _ = sink.send(Message::Close(None)).await;
                                    return String::new();
                                }
                            },
                            next = stream.next() => match next {
                                Some(Ok(Message::Text(text))) => {
                                    if let Ok(value) = serde_json::from_str::<Value>(&text) {
                                        if deliver.send(Incoming::Message(value)).is_err() {
                                            return String::new();
                                        }
                                        ctx.request_repaint();
                                    }
                                }
                                Some(Ok(Message::Close(frame))) => {
                                    return frame
                                        .map(|frame| frame.reason.to_string())
                                        .filter(|reason| !reason.is_empty())
                                        .unwrap_or_else(|| "The Jam ended.".to_string());
                                }
                                Some(Ok(_)) => {}
                                Some(Err(_)) | None => return "The Jam connection dropped.".to_string(),
                            },
                            _ = ping.tick() => {
                                let message = serde_json::json!({"type": "ping", "t": now_ms()});
                                if sink.send(Message::Text(message.to_string().into())).await.is_err() {
                                    return "The Jam connection dropped.".to_string();
                                }
                            }
                        }
                    }
                });
                let _ = deliver.send(Incoming::Closed(reason));
                ctx.request_repaint();
            })
            .expect("start the Jam thread");
        Self { outgoing, incoming }
    }

    pub fn send(&self, message: &Value) {
        let _ = self.outgoing.send(message.to_string());
    }

    pub fn try_recv(&self) -> Option<Incoming> {
        self.incoming.try_recv().ok()
    }
}

/// Milliseconds since the Unix epoch, as the relay counts them.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// A new Jam code: six letters and digits, leaving out ones that look alike.
pub fn new_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    (0..6)
        .map(|_| ALPHABET[rand::random_range(0..ALPHABET.len())] as char)
        .collect()
}

/// A code as typed ("k7q-4pm", "K7Q 4PM") in the relay's form, if it is one.
pub fn clean_code(typed: &str) -> Option<String> {
    let code: String = typed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (code.len() == 6).then_some(code)
}

/// A code shown to people: "K7Q-4PM".
pub fn show_code(code: &str) -> String {
    if code.len() == 6 {
        format!("{}-{}", &code[..3], &code[3..])
    } else {
        code.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_read_however_they_are_typed() {
        assert_eq!(clean_code("k7q-4pm").as_deref(), Some("K7Q4PM"));
        assert_eq!(clean_code(" K7Q 4PM ").as_deref(), Some("K7Q4PM"));
        assert_eq!(clean_code("K7Q4P"), None);
        assert_eq!(show_code("K7Q4PM"), "K7Q-4PM");
        let code = new_code();
        assert_eq!(clean_code(&code), Some(code.clone()));
    }
}
