mod quic;
mod mux;

pub use quic::QuicConnection;
pub use mux::MuxConnection;

use tokio::io::{AsyncRead, AsyncWrite};
use std::future::Future;

/// 统一的连接抽象
pub trait SnpConnection: Clone + Send + Sync + 'static {
    /// 双向流，同时支持读写
    type Stream: AsyncRead + AsyncWrite + Send + Unpin;

    /// 打开一个双向流，返回一个可读写的流
    fn open_bi(&self) -> impl Future<Output = Result<Self::Stream, Box<dyn std::error::Error + Send + Sync>>> + Send;

    /// 接受一个双向流
    fn accept_bi(&self) -> impl Future<Output = Result<Option<Self::Stream>, Box<dyn std::error::Error + Send + Sync>>> + Send;

    /// 连接关闭等待
    fn closed(&self) -> impl Future<Output = ()> + Send;
}