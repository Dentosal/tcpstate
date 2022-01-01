#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChannelState {
    /// Data is allowed to flow through
    Open,
    /// End of data on this channel has been reached, indicated by a FIN packet.
    ///
    /// On sending side, FIN is queued for sending and no more data will be accepted.
    ///
    /// On receiving side, FIN has been ACK and no more data will be arriving,
    /// but the buffer may still contain some data.
    Fin,
    /// FIN has been ACK'd. No more data remains in the buffer.
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// Server: Waiting for a connection request from any client.
    Listen,
    /// Client: SYN packet sent, waiting for SYN-ACK response.
    SynSent,
    /// Server: SYN packet received, SYN-ACK sent.
    SynReceived,
    /// Connection is open and operational, or in the process of closing down
    Established {
        tx_state: ChannelState,
        rx_state: ChannelState,
        /// User has waited for the connection to close
        close_called: bool,
        /// We called close before receiving FIN, so we must enter TimeWait before closing
        close_active: bool,
    },
    /// Waiting for to make sure the other side has time to close the connections.
    TimeWait,
    /// Connection has been terminated due to an reset, and the next call returns that
    Reset,
    /// Connection has been closed, or has never been opened at all. Initial state.
    Closed,
}
impl ConnectionState {
    pub const FULLY_OPEN: Self = Self::Established {
        tx_state: ChannelState::Open,
        rx_state: ChannelState::Open,
        close_called: false,
        close_active: false,
    };

    /// Check for some invalid transitions
    pub fn debug_validate_transition(self, to: ConnectionState) {
        if let ConnectionState::Established {
            tx_state,
            rx_state,
            close_called,
            close_active,
        } = self
        {
            if let ConnectionState::Established {
                tx_state: to_tx_state,
                rx_state: to_rx_state,
                close_called: to_close_called,
                close_active: to_close_active,
            } = to
            {
                debug_assert!(tx_state <= to_tx_state);
                debug_assert!(rx_state <= to_rx_state);
                debug_assert!(close_called <= to_close_called);
                debug_assert!(close_active <= to_close_active);
                if close_called {
                    debug_assert!(to_tx_state >= ChannelState::Fin);
                }
            }
        }
    }

    pub fn tx(self) -> ChannelState {
        match self {
            ConnectionState::SynSent | ConnectionState::SynReceived => ChannelState::Open,
            ConnectionState::Established { tx_state, .. } => tx_state,
            ConnectionState::Listen
            | ConnectionState::TimeWait
            | ConnectionState::Reset
            | ConnectionState::Closed => ChannelState::Closed,
        }
    }

    pub fn rx(self) -> ChannelState {
        match self {
            ConnectionState::SynSent | ConnectionState::SynReceived => ChannelState::Open,
            ConnectionState::Established { rx_state, .. } => rx_state,
            ConnectionState::Listen
            | ConnectionState::TimeWait
            | ConnectionState::Reset
            | ConnectionState::Closed => ChannelState::Closed,
        }
    }

    pub fn close_called(self) -> bool {
        match self {
            ConnectionState::SynSent | ConnectionState::SynReceived => false,
            ConnectionState::Established { close_called, .. } => close_called,
            ConnectionState::Listen
            | ConnectionState::TimeWait
            | ConnectionState::Reset
            | ConnectionState::Closed => panic!("Invalid in this context"),
        }
    }

    pub fn close_active(self) -> bool {
        match self {
            ConnectionState::SynSent | ConnectionState::SynReceived => false,
            ConnectionState::Established { close_active, .. } => close_active,
            ConnectionState::TimeWait => true,
            ConnectionState::Listen | ConnectionState::Reset | ConnectionState::Closed => {
                panic!("Invalid in this context")
            }
        }
    }

    /// Have we sent a FIN packet already?
    pub fn fin_sent(self) -> bool {
        debug_assert!(self != Self::Closed, "Why is this called in Closed state?");
        matches!(
            self,
            Self::Established {
                tx_state: ChannelState::Fin | ChannelState::Closed,
                ..
            } | Self::TimeWait
        )
    }

    /// Have we received a FIN packet
    pub fn fin_received(self) -> bool {
        debug_assert!(self != Self::Closed, "Why is this called in Closed state?");
        matches!(
            self,
            Self::Established {
                rx_state: ChannelState::Fin | ChannelState::Closed,
                ..
            } | Self::TimeWait
        )
    }

    /// Output buffer must be empty by now
    pub fn output_buffer_guaranteed_clear(self) -> bool {
        debug_assert!(self != Self::Closed, "Why is this called in Closed state?");
        matches!(
            self,
            Self::Established {
                rx_state: ChannelState::Closed,
                ..
            } | Self::TimeWait
        )
    }
}
