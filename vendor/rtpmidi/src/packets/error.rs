use thiserror::Error;

#[derive(Debug, Error)]
pub enum PacketParseError {
    #[error("Not enough data")]
    NotEnoughData,
    #[error("Invalid data")]
    InvalidData,
}
