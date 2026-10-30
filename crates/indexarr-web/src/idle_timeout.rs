//! Closes connections that stop making progress.
//!
//! `axum::serve` sets no timeouts, so a client that connects and never sends
//! anything holds its file descriptor forever. On a publicly reachable port
//! those connections pile up until the process runs out of descriptors.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::serve::Listener;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{Instant, Sleep};

/// A [`Listener`] whose connections fail once no bytes have moved in either
/// direction for `idle`.
pub struct IdleTimeoutListener<L> {
    inner: L,
    idle: Duration,
}

impl<L> IdleTimeoutListener<L> {
    pub fn new(inner: L, idle: Duration) -> Self {
        Self { inner, idle }
    }
}

impl<L: Listener> Listener for IdleTimeoutListener<L> {
    type Io = IdleTimeoutStream<L::Io>;
    type Addr = L::Addr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let (io, addr) = self.inner.accept().await;
        (IdleTimeoutStream::new(io, self.idle), addr)
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

pub struct IdleTimeoutStream<T> {
    inner: T,
    idle: Duration,
    deadline: Pin<Box<Sleep>>,
}

impl<T> IdleTimeoutStream<T> {
    fn new(inner: T, idle: Duration) -> Self {
        Self {
            inner,
            idle,
            deadline: Box::pin(tokio::time::sleep(idle)),
        }
    }

    fn touch(&mut self) {
        let next = Instant::now() + self.idle;
        self.deadline.as_mut().reset(next);
    }

    /// Called when the inner IO is pending, so the deadline's waker is registered.
    fn poll_expired<R>(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<R>> {
        match self.deadline.as_mut().poll(cx) {
            Poll::Ready(()) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "connection idle timeout",
            ))),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for IdleTimeoutStream<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        match Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                if buf.filled().len() > before {
                    self.touch();
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => self.poll_expired(cx),
        }
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for IdleTimeoutStream<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Ready(Ok(n)) => {
                if n > 0 {
                    self.touch();
                }
                Poll::Ready(Ok(n))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => self.poll_expired(cx),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::get;
    use axum::serve::ListenerExt;
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    const IDLE: Duration = Duration::from_millis(300);

    async fn spawn_server() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route("/health", get(|| async { "ok" }));
        tokio::spawn(async move {
            axum::serve(
                IdleTimeoutListener::new(listener, IDLE).tap_io(|_| {}),
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        addr
    }

    /// Reads until the server closes, failing if that takes far longer than `IDLE`.
    async fn read_to_close(stream: &mut TcpStream) -> Vec<u8> {
        let mut out = Vec::new();
        tokio::time::timeout(IDLE * 10, stream.read_to_end(&mut out))
            .await
            .expect("server did not close the connection")
            .ok();
        out
    }

    #[tokio::test]
    async fn closes_a_connection_that_never_sends_anything() {
        let addr = spawn_server().await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        assert!(read_to_close(&mut stream).await.is_empty());
    }

    #[tokio::test]
    async fn closes_an_idle_keep_alive_connection_after_serving_it() {
        let addr = spawn_server().await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: test\r\n\r\n")
            .await
            .unwrap();
        let response = read_to_close(&mut stream).await;
        assert!(response.starts_with(b"HTTP/1.1 200"));
    }

    #[tokio::test]
    async fn traffic_pushes_the_deadline_back() {
        let addr = spawn_server().await;
        let mut stream = TcpStream::connect(addr).await.unwrap();
        // Spans three idle periods, with no single gap reaching one.
        for chunk in ["GET /health HTTP/1.1\r\n", "Host: ", "test\r\n", "\r\n"] {
            tokio::time::sleep(IDLE * 3 / 4).await;
            stream.write_all(chunk.as_bytes()).await.unwrap();
        }
        let response = read_to_close(&mut stream).await;
        assert!(response.starts_with(b"HTTP/1.1 200"));
    }
}
