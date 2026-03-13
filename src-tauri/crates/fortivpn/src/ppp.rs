use std::net::Ipv4Addr;

// PPP protocol numbers
pub const LCP_PROTOCOL: u16 = 0xC021;
pub const IPCP_PROTOCOL: u16 = 0x8021;
pub const IP_PROTOCOL: u16 = 0x0021;

// LCP/IPCP codes
pub const LCP_CONFIGURE_REQUEST: u8 = 1;
pub const LCP_CONFIGURE_ACK: u8 = 2;
pub const LCP_CONFIGURE_NAK: u8 = 3;
pub const LCP_CONFIGURE_REJECT: u8 = 4;
pub const LCP_TERMINATE_REQUEST: u8 = 5;
pub const LCP_TERMINATE_ACK: u8 = 6;
pub const LCP_ECHO_REQUEST: u8 = 9;
pub const LCP_ECHO_REPLY: u8 = 10;

// LCP option types
const OPT_MRU: u8 = 1;
const OPT_ACCM: u8 = 2;
const OPT_MAGIC_NUMBER: u8 = 5;

// IPCP option types
pub const IPCP_OPT_IP_ADDRESS: u8 = 3;
pub const IPCP_OPT_PRIMARY_DNS: u8 = 129;
pub const IPCP_OPT_SECONDARY_DNS: u8 = 131;

#[derive(Debug, Clone)]
pub struct PppPacket {
    pub protocol: u16,
    pub code: u8,
    pub identifier: u8,
    pub data: Vec<u8>,
}

impl PppPacket {
    pub fn encode(&self) -> Vec<u8> {
        let length = (4 + self.data.len()) as u16;
        let mut buf = Vec::with_capacity(2 + 4 + self.data.len());
        buf.extend_from_slice(&self.protocol.to_be_bytes());
        buf.push(self.code);
        buf.push(self.identifier);
        buf.extend_from_slice(&length.to_be_bytes());
        buf.extend_from_slice(&self.data);
        buf
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < 6 {
            return Err(format!("PPP packet too short: {} bytes", bytes.len()));
        }
        let protocol = u16::from_be_bytes([bytes[0], bytes[1]]);
        let code = bytes[2];
        let identifier = bytes[3];
        let length = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
        let data_len = length.saturating_sub(4);
        if bytes.len() < 6 + data_len {
            return Err(format!(
                "PPP packet truncated: need {}, have {}",
                6 + data_len,
                bytes.len()
            ));
        }
        let data = bytes[6..6 + data_len].to_vec();
        Ok(Self {
            protocol,
            code,
            identifier,
            data,
        })
    }
}

#[derive(Debug, Clone)]
pub enum LcpOption {
    Mru(u16),
    Accm(u32),
    MagicNumber(u32),
    Unknown(u8, Vec<u8>),
}

impl LcpOption {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            LcpOption::Mru(mru) => {
                let mut buf = vec![OPT_MRU, 4];
                buf.extend_from_slice(&mru.to_be_bytes());
                buf
            }
            LcpOption::Accm(accm) => {
                let mut buf = vec![OPT_ACCM, 6];
                buf.extend_from_slice(&accm.to_be_bytes());
                buf
            }
            LcpOption::MagicNumber(magic) => {
                let mut buf = vec![OPT_MAGIC_NUMBER, 6];
                buf.extend_from_slice(&magic.to_be_bytes());
                buf
            }
            LcpOption::Unknown(typ, data) => {
                let mut buf = vec![*typ, (2 + data.len()) as u8];
                buf.extend_from_slice(data);
                buf
            }
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<(Self, usize), String> {
        if bytes.len() < 2 {
            return Err("Option too short".to_string());
        }
        let typ = bytes[0];
        let len = bytes[1] as usize;
        if len < 2 || bytes.len() < len {
            return Err(format!("Option length invalid: type={typ}, len={len}"));
        }
        let opt = match typ {
            OPT_MRU if len == 4 => LcpOption::Mru(u16::from_be_bytes([bytes[2], bytes[3]])),
            OPT_ACCM if len == 6 => {
                LcpOption::Accm(u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]))
            }
            OPT_MAGIC_NUMBER if len == 6 => {
                LcpOption::MagicNumber(u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]))
            }
            _ => LcpOption::Unknown(typ, bytes[2..len].to_vec()),
        };
        Ok((opt, len))
    }
}

#[derive(Debug, Clone)]
pub enum IpcpOption {
    IpAddress(std::net::Ipv4Addr),
    PrimaryDns(std::net::Ipv4Addr),
    SecondaryDns(std::net::Ipv4Addr),
    Unknown(u8, Vec<u8>),
}

impl IpcpOption {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            IpcpOption::IpAddress(ip) => {
                let mut buf = vec![IPCP_OPT_IP_ADDRESS, 6];
                buf.extend_from_slice(&ip.octets());
                buf
            }
            IpcpOption::PrimaryDns(ip) => {
                let mut buf = vec![IPCP_OPT_PRIMARY_DNS, 6];
                buf.extend_from_slice(&ip.octets());
                buf
            }
            IpcpOption::SecondaryDns(ip) => {
                let mut buf = vec![IPCP_OPT_SECONDARY_DNS, 6];
                buf.extend_from_slice(&ip.octets());
                buf
            }
            IpcpOption::Unknown(typ, data) => {
                let mut buf = vec![*typ, (2 + data.len()) as u8];
                buf.extend_from_slice(data);
                buf
            }
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<(Self, usize), String> {
        if bytes.len() < 2 {
            return Err("IPCP option too short".to_string());
        }
        let typ = bytes[0];
        let len = bytes[1] as usize;
        if len < 2 || bytes.len() < len {
            return Err(format!("IPCP option length invalid: type={typ}, len={len}"));
        }
        let opt = match typ {
            IPCP_OPT_IP_ADDRESS if len == 6 => IpcpOption::IpAddress(std::net::Ipv4Addr::new(
                bytes[2], bytes[3], bytes[4], bytes[5],
            )),
            IPCP_OPT_PRIMARY_DNS if len == 6 => IpcpOption::PrimaryDns(std::net::Ipv4Addr::new(
                bytes[2], bytes[3], bytes[4], bytes[5],
            )),
            IPCP_OPT_SECONDARY_DNS if len == 6 => IpcpOption::SecondaryDns(
                std::net::Ipv4Addr::new(bytes[2], bytes[3], bytes[4], bytes[5]),
            ),
            _ => IpcpOption::Unknown(typ, bytes[2..len].to_vec()),
        };
        Ok((opt, len))
    }
}

/// LCP negotiation state.
pub struct LcpState {
    pub magic_number: u32,
    pub mru: u16,
    pub opened: bool,
    pub peer_magic: u32,
}

impl LcpState {
    pub fn new() -> Self {
        Self {
            magic_number: rand::random::<u32>(),
            mru: 1354,
            opened: false,
            peer_magic: 0,
        }
    }

    pub fn build_configure_request(&self, identifier: u8) -> PppPacket {
        let mut data = Vec::new();
        data.extend_from_slice(&LcpOption::Mru(self.mru).encode());
        data.extend_from_slice(&LcpOption::MagicNumber(self.magic_number).encode());
        PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_CONFIGURE_REQUEST,
            identifier,
            data,
        }
    }

    pub fn handle_configure_request(&mut self, identifier: u8, options_data: &[u8]) -> PppPacket {
        let mut offset = 0;
        while offset < options_data.len() {
            if let Ok((opt, consumed)) = LcpOption::decode(&options_data[offset..]) {
                if let LcpOption::MagicNumber(m) = opt {
                    self.peer_magic = m;
                }
                offset += consumed;
            } else {
                break;
            }
        }
        PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_CONFIGURE_ACK,
            identifier,
            data: options_data.to_vec(),
        }
    }

    pub fn handle_configure_ack(&mut self) {
        self.opened = true;
    }

    pub fn handle_configure_nak(&mut self, options_data: &[u8]) {
        let mut offset = 0;
        while offset < options_data.len() {
            if let Ok((opt, consumed)) = LcpOption::decode(&options_data[offset..]) {
                match opt {
                    LcpOption::Mru(mru) => self.mru = mru,
                    _ => {}
                }
                offset += consumed;
            } else {
                break;
            }
        }
    }

    pub fn build_echo_request(&self, identifier: u8) -> PppPacket {
        PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_ECHO_REQUEST,
            identifier,
            data: self.magic_number.to_be_bytes().to_vec(),
        }
    }

    pub fn build_echo_reply(&self, identifier: u8) -> PppPacket {
        PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_ECHO_REPLY,
            identifier,
            data: self.magic_number.to_be_bytes().to_vec(),
        }
    }

    pub fn build_terminate_request(&self, identifier: u8) -> PppPacket {
        PppPacket {
            protocol: LCP_PROTOCOL,
            code: LCP_TERMINATE_REQUEST,
            identifier,
            data: vec![],
        }
    }
}

/// IPCP negotiation state.
pub struct IpcpState {
    pub local_ip: Ipv4Addr,
    pub primary_dns: Ipv4Addr,
    pub secondary_dns: Ipv4Addr,
    pub opened: bool,
}

impl IpcpState {
    pub fn new() -> Self {
        Self {
            local_ip: Ipv4Addr::UNSPECIFIED,
            primary_dns: Ipv4Addr::UNSPECIFIED,
            secondary_dns: Ipv4Addr::UNSPECIFIED,
            opened: false,
        }
    }

    pub fn build_configure_request(&self, identifier: u8) -> PppPacket {
        let mut data = Vec::new();
        data.extend_from_slice(&IpcpOption::IpAddress(self.local_ip).encode());
        data.extend_from_slice(&IpcpOption::PrimaryDns(self.primary_dns).encode());
        data.extend_from_slice(&IpcpOption::SecondaryDns(self.secondary_dns).encode());
        PppPacket {
            protocol: IPCP_PROTOCOL,
            code: LCP_CONFIGURE_REQUEST,
            identifier,
            data,
        }
    }

    pub fn handle_configure_request(&self, identifier: u8, options_data: &[u8]) -> PppPacket {
        PppPacket {
            protocol: IPCP_PROTOCOL,
            code: LCP_CONFIGURE_ACK,
            identifier,
            data: options_data.to_vec(),
        }
    }

    pub fn handle_configure_ack(&mut self) {
        self.opened = true;
    }

    pub fn handle_configure_nak(&mut self, options_data: &[u8]) {
        let mut offset = 0;
        while offset < options_data.len() {
            if let Ok((opt, consumed)) = IpcpOption::decode(&options_data[offset..]) {
                match opt {
                    IpcpOption::IpAddress(ip) => self.local_ip = ip,
                    IpcpOption::PrimaryDns(ip) => self.primary_dns = ip,
                    IpcpOption::SecondaryDns(ip) => self.secondary_dns = ip,
                    _ => {}
                }
                offset += consumed;
            } else {
                break;
            }
        }
    }
}
