use alloc::vec::Vec;

use crate::event;
use crate::segment::SegmentMeta;

#[derive(Debug, PartialEq)]
pub struct Res<T = ()> {
    pub value: Option<T>,
    pub unblock: Vec<event::Cookie>,
    pub error: Option<Error>,
}
impl<T> Res<T> {
    pub fn empty() -> Self {
        Self {
            value: None,
            unblock: Vec::new(),
            error: None,
        }
    }

    pub fn value(v: T) -> Self {
        Self {
            value: Some(v),
            unblock: Vec::new(),
            error: None,
        }
    }

    pub fn error(error: Error) -> Self {
        Self {
            value: None,
            unblock: Vec::new(),
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidStateTransition,
    NotConnected,
    ConnectionReset,
    ConnectionClosing,
}
