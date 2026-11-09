//! Minimal libpod REST client over a unix socket: four endpoints, HTTP/1.1.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use bytes::{Buf, Bytes};
use http_body_util::{BodyExt, Empty};
use hyper::body::Incoming;
use hyper::client::conn::http1::{self, SendRequest};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tracing::{debug, trace};

use crate::model::{DfReport, Event, ImageSummary, RemoveReport};

/// libpod routes are only registered under a version prefix. 4.0.0 is the
/// minimum every 4.x and 5.x server accepts.
const API: &str = "/v4.0.0/libpod";

#[derive(Debug, Clone)]
pub struct Client {
    socket: PathBuf,
}

/// What became of a `DELETE /libpod/images/{id}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveOutcome {
    Removed(RemoveReport),
    /// Already gone (404).
    NotFound,
    /// Used by a container or otherwise refused (409); carries the server message.
    InUse(String),
}

impl Client {
    pub fn new(socket: impl AsRef<Path>) -> Self {
        Client {
            socket: socket.as_ref().to_path_buf(),
        }
    }

    /// One connection per request: the API is local and calls are rare, and
    /// a fresh connection sidesteps any keep-alive state after a podman restart.
    async fn connect(&self) -> Result<SendRequest<Empty<Bytes>>> {
        let stream = UnixStream::connect(&self.socket)
            .await
            .with_context(|| format!("connecting to {}", self.socket.display()))?;
        let (sender, conn) = http1::handshake(TokioIo::new(stream)).await?;
        tokio::spawn(async move {
            if let Err(err) = conn.await {
                debug!(err = %format!("{err:#}"), "libpod connection closed");
            }
        });
        Ok(sender)
    }

    async fn send(&self, method: Method, path: &str) -> Result<hyper::Response<Incoming>> {
        let mut sender = self.connect().await?;
        let request = Request::builder()
            .method(method.clone())
            .uri(path)
            .header(hyper::header::HOST, "podman")
            .header(hyper::header::ACCEPT, "application/json")
            .body(Empty::<Bytes>::new())?;
        trace!(%method, path, "libpod request");
        let response = sender
            .send_request(request)
            .await
            .with_context(|| format!("{method} {path}"))?;
        Ok(response)
    }

    async fn call(&self, method: Method, path: &str) -> Result<(StatusCode, Bytes)> {
        let response = self.send(method.clone(), path).await?;
        let status = response.status();
        let body = response.into_body().collect().await?.to_bytes();
        trace!(%method, path, %status, len = body.len(), "libpod response");
        Ok((status, body))
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let (status, body) = self.call(Method::GET, path).await?;
        if !status.is_success() {
            bail!("GET {path}: {status}: {}", error_message(&body));
        }
        serde_json::from_slice(&body).with_context(|| format!("decoding GET {path}"))
    }

    pub async fn list_images(&self) -> Result<Vec<ImageSummary>> {
        self.get_json(&format!("{API}/images/json?all=true")).await
    }

    pub async fn df(&self) -> Result<DfReport> {
        self.get_json(&format!("{API}/system/df")).await
    }

    pub async fn remove_image(&self, id: &str, force: bool) -> Result<RemoveOutcome> {
        let path = format!("{API}/images/{id}?force={force}");
        let (status, body) = self.call(Method::DELETE, &path).await?;
        match status {
            StatusCode::OK => {
                let report: RemoveReport = serde_json::from_slice(&body)
                    .with_context(|| format!("decoding DELETE {path}"))?;
                Ok(RemoveOutcome::Removed(report))
            }
            StatusCode::NOT_FOUND => Ok(RemoveOutcome::NotFound),
            StatusCode::CONFLICT => Ok(RemoveOutcome::InUse(error_message(&body))),
            other => Err(anyhow!("DELETE {path}: {other}: {}", error_message(&body))),
        }
    }

    /// Open the event stream. `since` is a unix timestamp in seconds; the server
    /// replays events after it before following live ones.
    pub async fn events(&self, since: Option<u64>) -> Result<EventStream> {
        let mut path = format!("{API}/events?stream=true");
        if let Some(since) = since {
            path.push_str(&format!("&since={since}"));
        }
        let response = self.send(Method::GET, &path).await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.into_body().collect().await?.to_bytes();
            bail!("GET {path}: {status}: {}", error_message(&body));
        }
        Ok(EventStream {
            body: response.into_body(),
            buffer: Vec::new(),
            done: false,
        })
    }
}

/// Error bodies are `{"cause":..., "message":..., "response":...}`; fall back to raw text.
fn error_message(body: &Bytes) -> String {
    #[derive(serde::Deserialize)]
    struct ApiError {
        #[serde(default)]
        message: String,
        #[serde(default)]
        cause: String,
    }
    match serde_json::from_slice::<ApiError>(body) {
        Ok(err) if !err.message.is_empty() => err.message,
        Ok(err) if !err.cause.is_empty() => err.cause,
        _ => String::from_utf8_lossy(body).trim().to_string(),
    }
}

/// Newline-delimited JSON events over a long-lived response body.
pub struct EventStream {
    body: Incoming,
    buffer: Vec<u8>,
    done: bool,
}

impl EventStream {
    /// Next event, `None` when the server closed the stream.
    pub async fn next(&mut self) -> Result<Option<Event>> {
        loop {
            if let Some(event) = self.pop_line()? {
                return Ok(Some(event));
            }
            if self.done {
                return Ok(None);
            }
            match self.body.frame().await {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        trace!(len = data.len(), "event stream data");
                        self.buffer.extend_from_slice(data.chunk());
                    } else {
                        trace!("event stream trailers");
                    }
                }
                Some(Err(err)) => return Err(err.into()),
                None => self.done = true,
            }
        }
    }

    fn pop_line(&mut self) -> Result<Option<Event>> {
        loop {
            let Some(newline) = self.buffer.iter().position(|b| *b == b'\n') else {
                if self.done && !self.buffer.is_empty() {
                    let rest = std::mem::take(&mut self.buffer);
                    return parse_line(&rest);
                }
                return Ok(None);
            };
            let line: Vec<u8> = self.buffer.drain(..=newline).collect();
            if let Some(event) = parse_line(&line)? {
                return Ok(Some(event));
            }
        }
    }
}

fn parse_line(line: &[u8]) -> Result<Option<Event>> {
    let text = std::str::from_utf8(line)?.trim();
    if text.is_empty() {
        return Ok(None);
    }
    trace!(text, "event line");
    let event: Event =
        serde_json::from_str(text).with_context(|| format!("decoding event {text}"))?;
    Ok(Some(event))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_message_prefers_message_field() {
        let body = Bytes::from(
            r#"{"cause":"image is in use","message":"image used by abc: image is in use","response":409}"#,
        );
        assert_eq!(error_message(&body), "image used by abc: image is in use");
        assert_eq!(error_message(&Bytes::from("plain text\n")), "plain text");
    }

    #[test]
    fn parse_line_skips_blank() {
        assert!(parse_line(b"  \n").unwrap().is_none());
        let ev = parse_line(br#"{"Type":"image","Action":"pull","Actor":{"ID":"x"}}"#)
            .unwrap()
            .unwrap();
        assert_eq!(ev.status, "pull");
        assert!(parse_line(b"{broken").is_err());
    }
}
