#[derive(Debug, Clone, Default)]
pub struct Config {
    pub server_addr: String,
    pub server_name: String,
    pub auth: String,
    pub insecure: bool,
    pub bandwidth: BandwidthConfig,
    pub obfs_password: String,
    pub hop_ports: String,
    pub hop_interval_min_secs: u64,
    pub hop_interval_max_secs: u64,
    pub pin_sha256: String,
    pub fast_open: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BandwidthConfig {
    pub recv_bps: u64,
    pub send_bps: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerRecvRate {
    Missing,
    Auto,
    Unlimited,
    Limited(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NegotiatedBandwidth {
    pub client_recv_bps: u64,
    pub client_send_bps: u64,
    pub server_recv: ServerRecvRate,
    /// `None` means the sender must use the configured non-Brutal congestion
    /// controller instead of a fixed local transmit rate.
    pub fixed_send_bps: Option<u64>,
}

impl BandwidthConfig {
    pub fn negotiate(self, server_recv: ServerRecvRate) -> NegotiatedBandwidth {
        let fixed_send_bps = match (self.send_bps, server_recv) {
            (0, _) | (_, ServerRecvRate::Missing | ServerRecvRate::Auto) => None,
            (send_bps, ServerRecvRate::Unlimited) => Some(send_bps),
            (send_bps, ServerRecvRate::Limited(limit)) => Some(send_bps.min(limit)),
        };

        NegotiatedBandwidth {
            client_recv_bps: self.recv_bps,
            client_send_bps: self.send_bps,
            server_recv,
            fixed_send_bps,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiate_fixed_send_rate_against_server_limit() {
        let negotiated = BandwidthConfig {
            recv_bps: 100_000_000,
            send_bps: 30_000_000,
        }
        .negotiate(ServerRecvRate::Limited(20_000_000));

        assert_eq!(negotiated.client_recv_bps, 100_000_000);
        assert_eq!(negotiated.client_send_bps, 30_000_000);
        assert_eq!(negotiated.fixed_send_bps, Some(20_000_000));
    }

    #[test]
    fn negotiate_auto_server_rx_uses_congestion_controller() {
        let negotiated = BandwidthConfig {
            recv_bps: 100_000_000,
            send_bps: 30_000_000,
        }
        .negotiate(ServerRecvRate::Auto);

        assert_eq!(negotiated.fixed_send_bps, None);
    }
}
