use dbus::Message;
use dbus::nonblock::{MsgMatch, SyncConnection};
use futures::Stream;
use futures::channel::mpsc::UnboundedReceiver;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

/// Wrapper for a stream of D-Bus messages which automatically removes the `MsgMatch` from the D-Bus
/// connection when it is dropped.
pub struct MessageStream {
    msg_match: Option<MsgMatch>,
    events: UnboundedReceiver<Message>,
    connection: Arc<SyncConnection>,
}

impl MessageStream {
    pub fn new(msg_match: MsgMatch, connection: Arc<SyncConnection>) -> Self {
        let (msg_match, events) = msg_match.msg_stream();
        Self {
            msg_match: Some(msg_match),
            events,
            connection,
        }
    }
}

impl Stream for MessageStream {
    type Item = Message;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.events).poll_next(cx)
    }
}

impl Drop for MessageStream {
    fn drop(&mut self) {
        let connection = self.connection.clone();
        let msg_match = self.msg_match.take().unwrap();
        // kvm-it patch: the match may already be gone (D-Bus "No match with that id found"), e.g. when the
        // connection is closing. Upstream unwrap()s and the spawned task panics; removing a missing match is a no-op.
        tokio::spawn(async move {
            let _ = connection.remove_match(msg_match.token()).await;
        });
    }
}
