use std::time::Duration;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttentionEvent {
    Bell,
    CommandFinished {
        exit_status: Option<i32>,
        duration: Duration,
    },
}
