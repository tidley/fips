pub const ADVERT_KIND: u16 = 30078;
pub const TRAVERSAL_SIGNAL_KIND: u16 = 21059;
pub const INBOX_RELAYS_KIND: u16 = 10050;
pub const PUNCH_MAGIC: u32 = 0x4E505443;
pub const PUNCH_ACK_MAGIC: u32 = 0x4E505441;
pub const TRAVERSAL_SIGNAL_APP: &str = "fips.nat.traversal.v1";

pub const DEFAULT_ADVERT_RELAYS: &[&str] = &["wss://offchain.pub", "wss://strfry.bitsbytom.com"];

pub const DEFAULT_DM_RELAYS: &[&str] = &["wss://nip17.com", "wss://offchain.pub"];

pub const DEFAULT_STUN_SERVERS: &[&str] =
    &["stun:fips.tomdwyer.uk:3478", "stun:stun.l.google.com:19302"];
