//! Collapse only already-queued ordinary absolute motion. Never wait to form
//! a batch, cross an input/connection boundary, or discard drag samples.

use tokio::sync::mpsc;

use super::Work;
use crate::shared_input::InputCommand;

// Yield to the worker's cancellation/portal/EIS select branches between
// bounded batches, even if the producer continuously supplies motion.
const MAX_MOTION_BATCH: usize = 64;

pub(super) struct CommandReceiver {
    receiver: mpsc::Receiver<Work>,
    // The first non-coalescible command stays ordered ahead of the channel.
    // This single look-ahead slot is included when suspension drains input.
    pending: Option<Work>,
}

impl CommandReceiver {
    pub(super) fn new(receiver: mpsc::Receiver<Work>) -> Self {
        Self {
            receiver,
            pending: None,
        }
    }

    pub(super) async fn recv(&mut self) -> Option<Work> {
        let mut work = match self.pending.take() {
            Some(work) => work,
            None => self.receiver.recv().await?,
        };
        let Some(connection) = ordinary_motion_connection(&work) else {
            return Some(work);
        };
        // No await after removing work: this remains cancellation-safe inside
        // tokio::select!, just like mpsc::Receiver::recv().
        for _ in 1..MAX_MOTION_BATCH {
            match self.receiver.try_recv() {
                Ok(next) if ordinary_motion_connection(&next) == Some(connection) => {
                    work = next;
                }
                Ok(next) => {
                    self.pending = Some(next);
                    break;
                }
                Err(_) => break,
            }
        }
        Some(work)
    }

    /// Drain raw work, including a look-ahead key/button/disconnect, on pause.
    pub(super) fn try_recv(&mut self) -> Result<Work, mpsc::error::TryRecvError> {
        match self.pending.take() {
            Some(work) => Ok(work),
            None => self.receiver.try_recv(),
        }
    }
}

fn ordinary_motion_connection(work: &Work) -> Option<u64> {
    match work {
        Work::Input {
            connection,
            command: InputCommand::MouseMove {
                drag_button: None, ..
            },
        } => Some(*connection),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared_input::MouseButton;
    use futures_util::FutureExt;

    fn motion(connection: u64, x: i32) -> Work {
        Work::Input {
            connection,
            command: InputCommand::MouseMove {
                x,
                y: 10,
                drag_button: None,
            },
        }
    }

    fn queued(work: &[Work]) -> (mpsc::Sender<Work>, CommandReceiver) {
        let (sender, receiver) = mpsc::channel(work.len().max(1));
        for work in work {
            sender.try_send(work.clone()).unwrap();
        }
        (sender, CommandReceiver::new(receiver))
    }

    #[tokio::test]
    async fn motion_is_immediate_without_waiting_for_a_batch_or_timer() {
        let (sender, mut receiver) = queued(&[]);
        // Dropping a pending recv must not consume the next submitted motion.
        assert!(receiver.recv().now_or_never().is_none());
        sender.try_send(motion(1, 1)).unwrap();
        assert_eq!(receiver.recv().now_or_never(), Some(Some(motion(1, 1))));
        sender.try_send(motion(1, 2)).unwrap();
        assert_eq!(receiver.recv().now_or_never(), Some(Some(motion(1, 2))));
    }

    #[tokio::test]
    async fn a_motion_backlog_keeps_only_its_newest_position() {
        let work: Vec<_> = (0..64).map(|x| motion(7, x)).collect();
        let (_sender, mut receiver) = queued(&work);
        assert_eq!(receiver.recv().await, Some(motion(7, 63)));
        assert_eq!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    }

    #[tokio::test]
    async fn buttons_keys_scroll_releases_and_disconnects_are_ordering_barriers() {
        let commands = [
            InputCommand::MouseButton {
                button: MouseButton::Left,
                down: true,
                x: 2,
                y: 10,
            },
            InputCommand::MouseButton {
                button: MouseButton::Left,
                down: false,
                x: 2,
                y: 10,
            },
            InputCommand::Key {
                key_code: 0x41,
                down: true,
            },
            InputCommand::Key {
                key_code: 0x41,
                down: false,
            },
            InputCommand::Scroll {
                delta_x: 1,
                delta_y: -1,
            },
            InputCommand::ReleaseAll,
            InputCommand::SecureAttention,
            InputCommand::MouseMove {
                x: 2,
                y: 10,
                drag_button: Some(MouseButton::Left),
            },
        ];
        let mut barriers: Vec<_> = commands
            .into_iter()
            .map(|command| Work::Input {
                connection: 1,
                command,
            })
            .collect();
        barriers.push(Work::Disconnected(1));
        for barrier in barriers {
            let work = [
                motion(1, 1),
                motion(1, 2),
                barrier.clone(),
                motion(1, 3),
                motion(1, 4),
            ];
            let (_sender, mut receiver) = queued(&work);
            assert_eq!(receiver.recv().await, Some(motion(1, 2)));
            assert_eq!(receiver.recv().await, Some(barrier));
            assert_eq!(receiver.recv().await, Some(motion(1, 4)));
            assert_eq!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty));
        }
    }

    #[tokio::test]
    async fn coalescing_never_crosses_a_controller_connection_change() {
        let (_sender, mut receiver) = queued(&[
            motion(1, 1),
            motion(1, 2),
            motion(2, 3),
            motion(2, 4),
            motion(1, 5),
        ]);
        assert_eq!(receiver.recv().await, Some(motion(1, 2)));
        assert_eq!(receiver.recv().await, Some(motion(2, 4)));
        assert_eq!(receiver.recv().await, Some(motion(1, 5)));
    }

    #[tokio::test]
    async fn every_drag_sample_is_preserved() {
        let work: Vec<_> = (0..4)
            .map(|x| Work::Input {
                connection: 1,
                command: InputCommand::MouseMove {
                    x,
                    y: 10,
                    drag_button: Some(MouseButton::Left),
                },
            })
            .collect();
        let (_sender, mut receiver) = queued(&work);
        for work in work {
            assert_eq!(receiver.recv().await, Some(work));
        }
    }

    #[tokio::test]
    async fn a_continuous_backlog_returns_after_a_bounded_batch() {
        let work: Vec<_> = (0..(2 * MAX_MOTION_BATCH + 1) as i32)
            .map(|x| motion(1, x))
            .collect();
        let (_sender, mut receiver) = queued(&work);
        assert_eq!(
            receiver.recv().await,
            Some(motion(1, (MAX_MOTION_BATCH - 1) as i32))
        );
        assert_eq!(
            receiver.recv().await,
            Some(motion(1, (2 * MAX_MOTION_BATCH - 1) as i32))
        );
        assert_eq!(
            receiver.recv().await,
            Some(motion(1, (2 * MAX_MOTION_BATCH) as i32))
        );
    }

    #[tokio::test]
    async fn draining_includes_the_lookahead_command_before_later_channel_work() {
        let (_sender, mut receiver) = queued(&[motion(1, 1), Work::Disconnected(1), motion(2, 2)]);
        assert_eq!(receiver.recv().await, Some(motion(1, 1)));
        assert_eq!(receiver.try_recv(), Ok(Work::Disconnected(1)));
        assert_eq!(receiver.try_recv(), Ok(motion(2, 2)));
        assert_eq!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    }

    #[tokio::test]
    async fn sender_closure_preserves_the_buffered_barrier_and_finishes() {
        let (sender, mut receiver) = queued(&[motion(1, 1), Work::Disconnected(1), motion(2, 2)]);
        assert_eq!(receiver.recv().await, Some(motion(1, 1)));
        drop(sender);
        assert_eq!(receiver.recv().await, Some(Work::Disconnected(1)));
        assert_eq!(receiver.recv().await, Some(motion(2, 2)));
        assert_eq!(receiver.recv().await, None);
    }
}
