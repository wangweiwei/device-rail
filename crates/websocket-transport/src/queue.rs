use std::{sync::Arc, time::Duration};

use devicerail_protocol::EventStreamCursor;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

pub(crate) struct QueuedEvent {
    pub(crate) bytes: Vec<u8>,
    pub(crate) cursor: EventStreamCursor,
    _bytes: OwnedSemaphorePermit,
}

#[derive(Clone)]
pub(crate) struct EventQueueSender {
    sender: mpsc::Sender<QueuedEvent>,
    bytes: Arc<Semaphore>,
    stall_timeout: Duration,
}

pub(crate) struct EventQueueReceiver {
    receiver: mpsc::Receiver<QueuedEvent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueueError {
    Closed,
    SlowConsumer,
}

pub(crate) fn event_queue(
    max_events: usize,
    max_bytes: usize,
    stall_timeout: Duration,
) -> (EventQueueSender, EventQueueReceiver) {
    let (sender, receiver) = mpsc::channel(max_events);
    (
        EventQueueSender {
            sender,
            bytes: Arc::new(Semaphore::new(max_bytes)),
            stall_timeout,
        },
        EventQueueReceiver { receiver },
    )
}

impl EventQueueSender {
    pub(crate) async fn send(
        &self,
        bytes: Vec<u8>,
        cursor: EventStreamCursor,
    ) -> Result<(), QueueError> {
        let byte_count = u32::try_from(bytes.len()).map_err(|_| QueueError::SlowConsumer)?;
        let permit = tokio::time::timeout(
            self.stall_timeout,
            Arc::clone(&self.bytes).acquire_many_owned(byte_count),
        )
        .await
        .map_err(|_| QueueError::SlowConsumer)?
        .map_err(|_| QueueError::Closed)?;
        let slot = tokio::time::timeout(self.stall_timeout, self.sender.reserve())
            .await
            .map_err(|_| QueueError::SlowConsumer)?
            .map_err(|_| QueueError::Closed)?;
        slot.send(QueuedEvent {
            bytes,
            cursor,
            _bytes: permit,
        });
        Ok(())
    }
}

impl EventQueueReceiver {
    pub(crate) async fn recv(&mut self) -> Option<QueuedEvent> {
        self.receiver.recv().await
    }

    pub(crate) fn close(&mut self) {
        self.receiver.close();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use devicerail_protocol::{EventSequence, EventStreamCursor, EventStreamEpoch, SessionId};

    use super::{QueueError, event_queue};

    fn cursor(sequence: u64) -> EventStreamCursor {
        EventStreamCursor {
            stream_epoch: EventStreamEpoch::new(),
            session_id: SessionId::new(),
            sequence: EventSequence::new(sequence).expect("positive sequence"),
        }
    }

    #[tokio::test]
    async fn event_and_byte_budgets_stall_only_the_slow_subscriber() {
        let (sender, mut receiver) = event_queue(1, 4, Duration::from_millis(10));
        sender
            .send(vec![1, 2, 3, 4], cursor(1))
            .await
            .expect("first item fits both budgets");
        assert_eq!(
            sender.send(vec![5], cursor(2)).await,
            Err(QueueError::SlowConsumer)
        );
        drop(receiver.recv().await.expect("release first item permits"));
        sender
            .send(vec![5], cursor(2))
            .await
            .expect("budgets are released when the writer consumes an item");
        receiver.close();
        drop(receiver.recv().await);
        assert_eq!(
            sender.send(vec![6], cursor(3)).await,
            Err(QueueError::Closed)
        );
    }
}
